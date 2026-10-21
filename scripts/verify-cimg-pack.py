#!/usr/bin/env python3
"""Verify a CVITEK CIMG firmware pack BEFORE writing it to the board.

Reads a pack directory plus its partition_emmc.xml, and for every file the
pack is meant to write:

  - parses the CIMG container header (magic, version, chunk header size,
    chunk count, declared file_size, label)
  - checks the structure derived from the format:
        total_on_disk  = 64 + file_size
        file_size      = sum_of_payloads + nchunks * 64
  - checks the CRC32 of EVERY chunk against the CRC field of its own chunk
    header. The field index is found by MEASUREMENT (comparing zlib.crc32 of
    the payload against each of the 16 u32 words), never assumed.
  - prints each chunk's ABSOLUTE device offset (field 3), the value to compare
    against the partition map
  - inspects the payload (ext4 magic 53ef, kernel FIT magic d00dfeed)
  - writes sha256 plus the full chunk list to a JSON manifest beside this file

Exit 0 = everything checked. Exit != 0 = a problem was found; DO NOT WRITE.

Usage:
    python3 verify-cimg-pack.py [pack_directory]

Every field was measured on the official 0.2.4 pack (docs/emmc-layout.md).
"""

import hashlib
import json
import os
import re
import struct
import sys
import zlib

HDR = 64                                   # container header, and chunk header
CIMG = b"CIMG"


def read_container_header(f):
    """64-byte container header. Returns a dict, or None when it is not CIMG."""
    h = f.read(HDR)
    if len(h) < HDR or h[:4] != CIMG:
        return None
    ver, hdr_sz, nchunks, file_size = struct.unpack("<4I", h[4:20])
    label = h[20:36].split(b"\0")[0].decode("ascii", "replace")
    return {
        "version": ver,
        "chunk_hdr_sz": hdr_sz,
        "nchunks": nchunks,
        "file_size": file_size,
        "label": label,
    }


def read_chunk_header(f):
    """64-byte chunk header = 16 little-endian u32 words."""
    c = f.read(HDR)
    if len(c) < HDR:
        return None
    return struct.unpack("<16I", c)


def check_file(path):
    """Returns (report_dict, problem_count)."""
    size = os.path.getsize(path)
    rep = {
        "file": os.path.basename(path),
        "size_on_disk": size,
        "problems": [],
        "chunks": [],
    }

    with open(path, "rb") as f:
        hdr = read_container_header(f)

        if hdr is None:
            # Not a CIMG container: a raw image (fip.bin). Digest and head only.
            f.seek(0)
            rep["container"] = "raw (no CIMG)"
            rep["leading_bytes"] = f.read(16).hex()
            f.seek(0)
            rep["sha256"] = hashlib.sha256(f.read()).hexdigest()
            return rep, 0

        rep["container"] = "CIMG"
        rep.update(hdr)

        # ---- structure implied by the format ---------------------------
        expected_total = HDR + hdr["file_size"]
        if expected_total != size:
            rep["problems"].append(
                "structure: 64 + file_size = %d, file is %d bytes"
                % (expected_total, size))
        rep["total_on_disk"] = expected_total

        payload_sum = 0
        pos = HDR
        crc_index = None
        crc_name = None

        for i in range(hdr["nchunks"]):
            f.seek(pos)
            words = read_chunk_header(f)
            if words is None:
                rep["problems"].append("chunk %d: truncated header" % i)
                break

            chunk_size, chunk_offset, part_size = words[1], words[2], words[3]
            data = f.read(chunk_size)
            if len(data) != chunk_size:
                rep["problems"].append(
                    "chunk %d: truncated payload (%d of %d bytes)"
                    % (i, len(data), chunk_size))
                break
            payload_sum += chunk_size

            crc = zlib.crc32(data) & 0xFFFFFFFF
            if crc_index is None:
                # Find, by measurement, which word carries the CRC.
                for k, v in enumerate(words):
                    if v == crc:
                        crc_index, crc_name = k, "f%d" % k
                        break
            if crc_index is None:
                rep["problems"].append(
                    "chunk %d: CRC32 %08x matches no header word" % (i, crc))
            elif words[crc_index] != crc:
                rep["problems"].append(
                    "chunk %d: payload CRC32 %08x != %s in header %08x"
                    % (i, crc, crc_name, words[crc_index]))

            rep["chunks"].append({
                "i": i,
                "size": chunk_size,
                "device_offset": chunk_offset,
                "part_size": part_size,
                "crc32_computed": "%08x" % crc,
                "crc32_in_header": "%08x" % words[crc_index] if crc_index is not None else None,
            })
            pos += HDR + chunk_size

        rep["crc_field"] = crc_name
        rep["payload_sum"] = payload_sum

        expected_payload = hdr["file_size"] - hdr["nchunks"] * HDR
        if payload_sum != expected_payload:
            rep["problems"].append(
                "payloads sum to %d, but file_size - nchunks*64 = %d"
                % (payload_sum, expected_payload))
        if pos != size:
            rep["problems"].append(
                "chunks end at %d, file is %d bytes" % (pos, size))

        # ---- look inside the first payload ------------------------------
        f.seek(HDR + HDR)
        head = f.read(4096 + 0x40)
        if head[:4] == b"\xd0\x0d\xfe\xed":
            rep["payload"] = "kernel FIT (magic d00dfeed)"
        elif len(head) > 1024 + 0x3A and \
                struct.unpack_from("<H", head, 1024 + 0x38)[0] == 0xEF53:
            blocks, = struct.unpack_from("<I", head, 1024 + 4)
            log_block, = struct.unpack_from("<I", head, 1024 + 24)
            rep["payload"] = "ext4 (magic 53ef), fs = %d bytes" % (
                blocks * (1024 << log_block))
        else:
            rep["payload"] = "unknown (first bytes: %s)" % head[:8].hex()

        f.seek(0)
        rep["sha256"] = hashlib.sha256(f.read()).hexdigest()

    return rep, len(rep["problems"])


def files_from_xml(pack_dir):
    xml = os.path.join(pack_dir, "partition_emmc.xml")
    if not os.path.isfile(xml):
        return None
    text = open(xml, encoding="utf-8", errors="replace").read()
    return [m for m in re.findall(r'file="([^"]*)"', text) if m]


def main():
    pack = sys.argv[1] if len(sys.argv) > 1 else os.path.expanduser(
        "~/recamera/oficial/emmc-usbdl")

    print("pack     : %s" % pack)
    files = files_from_xml(pack)
    if files is None:
        print("ERROR: no partition_emmc.xml in this directory")
        return 2
    print("to write : %s" % (files or "(nothing)"))
    print()

    manifest = {"pack": pack, "files": []}
    problems = 0

    for name in files:
        path = os.path.join(pack, name)
        if not os.path.isfile(path):
            print("ERROR: %s is not in the pack" % name)
            problems += 1
            continue

        rep, n = check_file(path)
        problems += n
        print("--- %s  (%d bytes)  %s"
              % (name, rep["size_on_disk"], "OK" if n == 0 else "%d PROBLEM(S)" % n))
        print("    container : %s" % rep.get("container"))

        if rep.get("container") == "CIMG":
            print("    label     : %r   version %s   nchunks %s   file_size %s"
                  % (rep.get("label"), rep.get("version"),
                     rep.get("nchunks"), rep.get("file_size")))
            print("    on disk   : 64 + file_size = %s" % rep.get("total_on_disk"))
            print("    CRC field : %s" % rep.get("crc_field"))
            print("    payload   : %s" % rep.get("payload"))
            if rep["chunks"]:
                print("    first chunk: device offset %s   size %s"
                      % (rep["chunks"][0]["device_offset"], rep["chunks"][0]["size"]))
        else:
            print("    leading   : %s" % rep.get("leading_bytes"))

        print("    sha256    : %s" % rep.get("sha256"))
        for p in rep["problems"]:
            print("    !! %s" % p)
        manifest["files"].append(rep)

    manifest["problems"] = problems
    out = os.path.join(os.path.dirname(os.path.abspath(__file__)),
                       "pack-manifest.json")
    with open(out, "w", encoding="utf-8") as f:
        json.dump(manifest, f, indent=1, ensure_ascii=False)

    print()
    print("manifest : %s" % out)
    print("RESULT   : %s" % ("ALL CHECKED" if problems == 0
                             else "%d PROBLEM(S) - DO NOT WRITE" % problems))
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())

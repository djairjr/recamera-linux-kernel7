#!/usr/bin/env python3
"""Pack the Buildroot artifacts into the eMMC pack the SG2002 USB downloader eats.

The pack is the one the vendor ships (`oficial/emmc-usbdl/`): partition_emmc.xml
plus the images the layout names, each wrapped in the CVITEK "CIMG" container.
`usb_dl` walks that XML and writes every file at the offset the container header
carries, which is why the images cannot be copied by hand.

Format, measured (never assumed) from the shipped 0.2.4 pack with
`verify-cimg-pack.py`; every field below was read off real headers:

  container header  64 B = 16 u32 little-endian
      [0] "CIMG"   [1] version=1   [2] header size=64   [3] nchunks
      [4] file_size = sum(payloads) + nchunks*64
      [5..8] label, 16 B ASCII NUL-padded ("BOOT", "ROOTFS")
      [9..15] 0
  chunk header      64 B = 16 u32 little-endian
      [0] 1            [1] this chunk's payload size
      [2] absolute device offset of this chunk, in bytes
      [3] bytes of the partition written up to and including this chunk
      [4] CRC32 (zlib) of this chunk's payload
      [5..15] 0
  chunk payload     raw bytes; the rootfs is split into 16 MiB chunks

A plain file (fip.bin) has no CIMG wrapper: `verify-cimg-pack.py` digests it
as-is, and `usb_dl` writes it at the offset the XML assigns.

Layout (partition_emmc.xml of the vendor, offsets in bytes):
  BOOT     0         8192 KiB   boot.emmc
  ENV      8388608    128 KiB   (empty; u-boot env, 0x40000 alignment)
  ROOTFS   8519680  524288 KiB  rootfs_ext4.emmc (ext4)
  ROOTFS2  ...      524288 KiB  (empty)
  RESERVED ...      262144 KiB  (empty)
  USERDATA ...      the rest    (empty)

Usage:  pack-emmc-pack.py [--images DIR] [--fip FILE] [--out DIR] [--verify]
"""

import argparse
import hashlib
import os
import struct
import sys
import zlib

HDR = 64
CHUNK = 16 * 1024 * 1024          # the vendor splits the rootfs in 16 MiB chunks
BOOT_OFFSET = 0                   # BOOT is the first partition: the pack says 0
BOOT_SIZE = 8192 * 1024
ENV_OFFSET = BOOT_OFFSET + BOOT_SIZE
ENV_SIZE = 128 * 1024
ROOTFS_OFFSET = ENV_OFFSET + ENV_SIZE     # 8519680, measured in the pack
ROOTFS_SIZE = 524288 * 1024


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for b in iter(lambda: f.read(1 << 20), b""):
            h.update(b)
    return h.hexdigest()


def md5(path):
    h = hashlib.md5()
    with open(path, "rb") as f:
        for b in iter(lambda: f.read(1 << 20), b""):
            h.update(b)
    return h.hexdigest()


def pack_cimg(raw_path, label, dev_offset, part_total, out_path, chunk=CHUNK):
    """Wrap one raw image in the CIMG container, chunk by chunk."""
    size = os.path.getsize(raw_path)
    nchunks = max(1, (size + chunk - 1) // chunk)
    file_size = size + nchunks * HDR

    with open(out_path, "wb") as out:
        out.write(struct.pack("<4I", 0x474D4943, 1, HDR, nchunks))   # "CIMG"
        out.write(struct.pack("<I", file_size))
        out.write(label.encode("ascii")[:16].ljust(16, b"\0"))
        out.write(b"\0" * (HDR - 20 - 16))

        with open(raw_path, "rb") as f:
            done = 0
            for i in range(nchunks):
                data = f.read(chunk)
                if not data:
                    raise SystemExit("chunk %d: no data at offset %d" % (i, done))
                written = done + len(data)
                # [3] is the partition bytes written up to AND INCLUDING this chunk
                part_now = min(written, part_total) if part_total else written
                out.write(struct.pack("<16I",
                                      1,
                                      len(data),
                                      dev_offset + done,
                                      part_now,
                                      zlib.crc32(data) & 0xFFFFFFFF,
                                      *([0] * 11)))
                out.write(data)
                done = written
    return nchunks, file_size


def main():
    ap = argparse.ArgumentParser()
    here = os.path.dirname(os.path.abspath(__file__))
    root = os.path.dirname(os.path.dirname(here))          # <project>
    ap.add_argument("--images", default=os.path.join(root, "output/sg2002_recamera_emmc/images"))
    ap.add_argument("--fip", default=os.path.join(root, "output/sg2002_recamera_emmc/images/fip.bin"))
    ap.add_argument("--out", default=os.path.join(root, "output/sg2002_recamera_emmc/emmc-pack"))
    ap.add_argument("--verify", action="store_true", help="run verify-cimg-pack.py on the result")
    args = ap.parse_args()

    boot = os.path.join(args.images, "boot.emmc")
    rootfs = os.path.join(args.images, "rootfs.ext4")
    if os.path.islink(rootfs):
        rootfs = os.path.realpath(rootfs)
    for p in (boot, rootfs):
        if not os.path.isfile(p):
            raise SystemExit("missing: %s (run `make -j1 all` in the container first)" % p)
    if not os.path.isfile(args.fip):
        raise SystemExit("missing fip: %s" % args.fip)

    boot_size, rootfs_size = os.path.getsize(boot), os.path.getsize(rootfs)
    if boot_size > BOOT_SIZE:
        raise SystemExit("boot.emmc is %d B, larger than the BOOT partition (%d B)"
                         % (boot_size, BOOT_SIZE))
    if rootfs_size > ROOTFS_SIZE:
        raise SystemExit("rootfs is %d B, larger than the ROOTFS partition (%d B)"
                         % (rootfs_size, ROOTFS_SIZE))

    os.makedirs(args.out, exist_ok=True)
    os.makedirs(os.path.join(args.out, "META"), exist_ok=True)

    n, fs = pack_cimg(boot, "BOOT", BOOT_OFFSET, BOOT_SIZE,
                      os.path.join(args.out, "boot.emmc"))
    print("boot.emmc      %9d B  -> CIMG %9d B  nchunks %d  offset %d" %
          (boot_size, HDR + fs, n, BOOT_OFFSET))
    n, fs = pack_cimg(rootfs, "ROOTFS", ROOTFS_OFFSET, 0,
                      os.path.join(args.out, "rootfs_ext4.emmc"))
    print("rootfs_ext4    %9d B  -> CIMG %9d B  nchunks %d  offset %d" %
          (rootfs_size, HDR + fs, n, ROOTFS_OFFSET))

    with open(args.fip, "rb") as f, open(os.path.join(args.out, "fip.bin"), "wb") as o:
        o.write(f.read())
    print("fip.bin        %9d B  (raw, no CIMG)" % os.path.getsize(args.fip))

    xml = """<physical_partition type="emmc">
\t<partition label="BOOT" size_in_kb="8192" file="boot.emmc" />
\t<!-- Beware that in emmc u-boot environment should be 0x40000 alignment -->
\t<partition label="ENV" size_in_kb="128" file="" />
\t<partition label="ROOTFS" size_in_kb="524288" file="rootfs_ext4.emmc" type="ext4"/>
\t<partition label="ROOTFS2" size_in_kb="524288" file="" type="ext4"/>
\t<partition label="RESERVED" size_in_kb="262144" file="" type="ext4"/>
\t<partition label="USERDATA" file="" type="ext4"/>
</physical_partition>
"""
    with open(os.path.join(args.out, "partition_emmc.xml"), "w") as f:
        f.write(xml)
    with open(os.path.join(args.out, "META", "misc_info.txt"), "w") as f:
        f.write("Version=1\nSTORAGE_TYPE=emmc\nCHIP=sg2002\nBOARD=recamera_emmc\n")

    files = ["boot.emmc", "rootfs_ext4.emmc", "fip.bin", "partition_emmc.xml",
             "META/misc_info.txt"]
    lines = []
    for rel in files:
        lines.append("%s  %s" % (md5(os.path.join(args.out, rel)), rel))
    with open(os.path.join(args.out, "META", "metadata.txt"), "w") as f:
        f.write("\n".join(lines) + "\n")

    with open(os.path.join(args.out, "SHA256SUMS"), "w") as f:
        for rel in files:
            f.write("%s  %s\n" % (sha256(os.path.join(args.out, rel)), rel))

    print("\npack in %s" % args.out)
    for rel in files:
        print("  %-24s %10d B  md5 %s" %
              (rel, os.path.getsize(os.path.join(args.out, rel)),
               md5(os.path.join(args.out, rel))))

    if args.verify:
        verifier = os.path.join(os.path.dirname(os.path.abspath(__file__)),
                                "verify-cimg-pack.py")
        if not os.path.isfile(verifier):
            print("\n(no verifier at %s -- skipped)" % verifier)
        else:
            print("\n--- verify-cimg-pack.py %s" % args.out)
            sys.exit(os.system("%s %s %s" % (sys.executable, verifier, args.out)))


if __name__ == "__main__":
    main()

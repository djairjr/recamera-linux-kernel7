#!/bin/sh
# SPDX-License-Identifier: GPL-2.0
#
# make-fip.sh -- rebuild the board fip.bin from the official one, replacing
# only its OpenSBI (MONITOR) with the vendor OpenSBI 0.9 plus the fence.tso
# emulation patch. Everything else (FSBL, DDR parameters, the second core's
# firmware, U-Boot) stays byte for byte the official one.
#
# Why: the SG2002's T-Head C906 traps on fence.tso (illegal instruction)
# instead of running it as the fence rw,rw its encoding falls back to. The
# patch (Armbian, patch/opensbi/sophgo-sg200x) makes OpenSBI execute a full
# fence rw,rw -- a stronger barrier -- and step over it. OpenSBI 1.7 upstream
# carries the same code; the fip shipped by Seeed has OpenSBI 0.9 without it.
# docs/multimedia.md §6.2.
#
# Checks, each fatal:
#  1. the unpatched vendor OpenSBI, built with the vendor (T-Head) toolchain
#     and the device tree embedded in the official MONITOR, is byte-identical
#     to the official MONITOR;
#  2. repacking the official fip with that unpatched OpenSBI gives the
#     official fip back, byte for byte;
#  3. the patched fip differs only inside MONITOR and in two checksums.
#
# Usage: make-fip.sh <work dir>   (writes <work dir>/fip.bin)
#   Not run by the build (fip.bin is checked in); to rebuild it, inside the
#   build container: SDK=/path/reCamera-OS make-fip.sh /work/work/fip-work
# Environment: SDK (a reCamera-OS checkout, required), CROSS (the
#   SDK's T-Head riscv64 toolchain prefix), ORIG and PATCH (next to this script).

set -eu

HERE=$(cd "$(dirname "$0")" && pwd)
SDK=${SDK:?set SDK to a Seeed-Studio/reCamera-OS checkout with its submodules}
ORIG=${ORIG:-$HERE/fip-seeed-original.bin}
PATCH=${PATCH:-$HERE/0001-lib-sbi_illegal_insn-add-emulation-for-fence-tso.patch}
FIPTOOL=$SDK/fsbl/plat/cv181x/fiptool.py
CROSS=${CROSS:-$SDK/host-tools/gcc/riscv64-linux-x86_64/bin/riscv64-unknown-linux-gnu-}
W=${1:?usage: make-fip.sh <work dir>}

mkdir -p "$W"
W=$(cd "$W" && pwd)

# MONITOR (OpenSBI fw_dynamic) of the official fip, and its embedded FDT
python3 -I - "$FIPTOOL" "$ORIG" "$W" <<'EOF'
import importlib.util, struct, sys
spec = importlib.util.spec_from_file_location("fiptool", sys.argv[1])
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)
fip = m.FIP()
fip.read_fip(sys.argv[2])
mon = fip.body2["MONITOR"].content
open(sys.argv[3] + "/monitor-orig.bin", "wb").write(mon)
i = mon.find(b"\xd0\x0d\xfe\xed")
n = struct.unpack(">I", mon[i + 4:i + 8])[0]
open(sys.argv[3] + "/monitor-fdt.dtb", "wb").write(mon[i:i + n])
EOF

build_sbi() { # <dir> [patch]
	rm -rf "$1"
	cp -a "$SDK/opensbi" "$1"
	[ -z "${2:-}" ] || patch -d "$1" -p1 -s < "$2"
	make -C "$1" -j"$(nproc)" -s CHIP_ARCH=CV181X CROSS_COMPILE="$CROSS" \
		PLATFORM=generic CONFIG_OPENSBI_FASTBOOT= \
		FW_FDT_PATH="$W/monitor-fdt.dtb" > "$1.log" 2>&1
}

build_sbi "$W/sbi-orig"
build_sbi "$W/sbi-fencetso" "$PATCH"
FW=build/platform/generic/firmware/fw_dynamic.bin

# 1. reproducible: the rebuilt vendor OpenSBI is the official one
python3 -I - "$W/sbi-orig/$FW" "$W/monitor-orig.bin" <<'EOF'
import sys
a = open(sys.argv[1], "rb").read()
b = open(sys.argv[2], "rb").read()
if b[:len(a)] != a or any(b[len(a):]):
    sys.exit("check 1 failed: rebuilt OpenSBI differs from the official MONITOR")
EOF

genfip() { # <monitor> <out>
	python3 -I "$FIPTOOL" genfip --OLD_FIP "$ORIG" --MONITOR "$1" \
		--MONITOR_RUNADDR 0x80000000 "$2" > /dev/null
}

# 2. repacking with the unpatched OpenSBI gives the official fip back
genfip "$W/sbi-orig/$FW" "$W/fip-repack.bin"
cmp -s "$ORIG" "$W/fip-repack.bin" || { echo "check 2 failed: repack differs" >&2; exit 1; }

# 3. the patched fip: differences only in MONITOR (0x1e800..0x3b000) and the
#    PARAM2 / MONITOR checksums (0xa608, 0xa630)
genfip "$W/sbi-fencetso/$FW" "$W/fip.bin"
cmp -l "$ORIG" "$W/fip.bin" | awk '
	{ o = $1 - 1
	  if (!((o >= 124928 && o < 241664) || o == 42504 || o == 42505 || o == 42544 || o == 42545)) bad++ }
	END { if (bad) { print "check 3 failed: " bad " bytes changed outside MONITOR"; exit 1 } }'

sha256sum "$ORIG" "$W/fip.bin"
echo "OK: $W/fip.bin"

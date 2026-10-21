#!/bin/sh
# SPDX-License-Identifier: GPL-2.0
#
# port.sh -- edit a vendor port patch of a local Sophgo package, keeping the
# working copies inside this tree (work/<package>/) instead of /tmp.
#
#   scripts/port.sh init  <package> [patch]   a/ = vendor files the package
#                                             patches touch + the patches before
#                                             <patch>; b/ = a/ + <patch>
#   scripts/port.sh add   <package> <path>... bring more vendor files into a/ and b/
#   scripts/port.sh diff  <package>           regenerate <patch> from a/ b/
#                                             (header kept) and check that the
#                                             whole series applies to the vendor
#   scripts/port.sh build sophgo-osdrv        quick out-of-tree module build of
#                                             b/ against the built 7.2 kernel
#                                             (inside the build container)
#
# <patch> is a file name in external/package/<package>/ (default: the last one).
# A new patch: write its header (From:, Subject:, description) into
# external/package/<package>/NNNN-name.patch, then "init <package> NNNN-name.patch".
# Edit the files under work/<package>/b/, then "diff". Paths are relative to
# the package build directory, as the patches (-p1) expect.
set -eu

HERE=$(cd "$(dirname "$0")" && pwd)
P=$(cd "$HERE/.." && pwd)
# a checkout of Seeed-Studio/reCamera-OS with its submodules (the vendor
# sources the port patches apply to); only this script needs it, the build
# downloads its sources
V=${RECAMERA_OS:-$P/vendor/reCamera-OS}
[ -d "$V" ] || { echo "port.sh: set RECAMERA_OS to a reCamera-OS checkout (with submodules)" >&2; exit 1; }

cmd=${1:?usage: port.sh init|add|diff|build <package> ...}
pkg=${2:?package name}
shift 2
PKGDIR=$P/external/package/$pkg
W=$P/work/$pkg
[ -d "$PKGDIR" ] || { echo "no package $pkg" >&2; exit 1; }

case "$pkg" in
	sophgo-osdrv) ROOT=$V/osdrv/interdrv ;;
	sophgo-ion) ROOT=$V/linux_5.10/drivers/staging/android ;;
	sophgo-cviruntime) ROOT=$V/cviruntime ;;
	sophgo-mpi) ROOT=$V/cvi_mpi ;;
	*) echo "port.sh: $pkg is not a vendor port package" >&2; exit 1 ;;
esac

# vendor file for a path of the package build directory
vendor_file() {
	case "$pkg:$1" in
		# sophgo-mpi copies the OV5647 sensor sources into its tree
		sophgo-mpi:component/isp/common/*) echo "$V/SensorSupportList/common/${1#component/isp/common/}" ;;
		sophgo-mpi:component/isp/sensor/*) echo "$V/SensorSupportList/sensor/${1#component/isp/sensor/}" ;;
		*) echo "$ROOT/$1" ;;
	esac
}

# files a patch touches (+++ b/<path>), new files included
patch_files() {
	sed -n 's|^+++ b/\([^\t ]*\).*|\1|p' "$1"
}

patches() {
	ls "$PKGDIR"/*.patch 2>/dev/null | sort
}

copy_vendor() { # <dest dir> <path>...
	d=$1; shift
	for f in "$@"; do
		src=$(vendor_file "$f")
		[ -e "$src" ] || continue        # created by a patch
		mkdir -p "$d/$(dirname "$f")"
		cp -a "$src" "$d/$f"
	done
}

target_patch() {
	if [ -f "$W/.patch" ]; then cat "$W/.patch"; else patches | tail -1; fi
}

# a fresh copy of the vendor files of every patch, with the patches up to
# (and excluding) $2 applied, into $1
base_tree() {
	rm -rf "$1"; mkdir -p "$1"
	all=""
	for p in $(patches); do all="$all $(patch_files "$p")"; done
	extra=""; [ -f "$W/.extra" ] && extra=$(cat "$W/.extra")
	copy_vendor "$1" $all $extra
	for p in $(patches); do
		[ "$p" = "$2" ] && break
		has_diff "$p" && patch -d "$1" -p1 -s < "$p"
	done
	return 0
}

# a new patch starts as its header alone
has_diff() {
	grep -q '^--- ' "$1"
}

case "$cmd" in
	init)
		tp=${1:+$PKGDIR/$1}; tp=${tp:-$(patches | tail -1)}
		[ -f "$tp" ] || { echo "no patch $tp" >&2; exit 1; }
		mkdir -p "$W"; echo "$tp" > "$W/.patch"
		base_tree "$W/a" "$tp"
		rm -rf "$W/b"; cp -a "$W/a" "$W/b"
		has_diff "$tp" && patch -d "$W/b" -p1 -s < "$tp"
		echo "work/$pkg: a/ = vendor + patches before $(basename "$tp"), b/ = a/ + it; edit b/"
		;;
	add)
		[ -d "$W/b" ] || { echo "run init first" >&2; exit 1; }
		for f in "$@"; do
			[ -e "$W/a/$f" ] && { echo "already there: $f"; continue; }
			copy_vendor "$W/a" "$f"; copy_vendor "$W/b" "$f"
			echo "$f" >> "$W/.extra"
		done
		;;
	diff)
		tp=$(target_patch)
		hdr=$(mktemp); out=$(mktemp)
		awk '/^(diff |--- )/ { exit } { print }' "$tp" > "$hdr"
		# no timestamps: the same edit always gives the same patch
		(cd "$W" && LC_ALL=C diff -ruN a b) | sed 's/^\(---\|+++\) \([^\t]*\)\t.*/\1 \2/' > "$out"
		cat "$hdr" "$out" > "$tp"
		rm -f "$hdr" "$out"
		# the whole series must apply to the vendor files, in order
		chk=$W/.check
		base_tree "$chk" "/none"
		rm -rf "$chk"
		echo "$(basename "$tp"): $(grep -c '^diff ' "$tp") files, series applies"
		;;
	build)
		[ "$pkg" = sophgo-osdrv ] || { echo "build: only sophgo-osdrv" >&2; exit 1; }
		B=$W/build
		mkdir -p "$B"
		rsync -a --delete --exclude '*.o' --exclude '*.ko' --exclude '.*.cmd' "$ROOT/" "$B/"
		rsync -a "$W/b/" "$B/"
		cp "$PKGDIR/Kbuild" "$B/Kbuild"; rm -f "$B/Makefile"
		cp -r "$PKGDIR/kernel-5.10-headers" "$B/"
		O=/work/output/sg2002_recamera_emmc
		"$P/container/run.sh" sh -c "export PATH=$O/host/bin:\$PATH; \
			ION=\$(ls -d $O/build/sophgo-ion-*); \
			make -C $O/build/linux-7.2 ARCH=riscv CROSS_COMPILE=riscv64-buildroot-linux-musl- \
			M=/work/work/$pkg/build SOPHGO_ION_DIR=\$ION KBUILD_EXTRA_SYMBOLS=\$ION/Module.symvers \
			modules -j\$(nproc) $*"
		ls -la "$B"/*.ko
		;;
	*)
		echo "usage: port.sh init|add|diff|build <package> ..." >&2; exit 2 ;;
esac

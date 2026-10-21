#!/bin/sh
# SPDX-License-Identifier: GPL-2.0
#
# slot-update.sh -- write the current build (images/rootfs.ext4 + boot.itb)
# to the INACTIVE eMMC slot of the camera over ssh, verify, resize, label.
#
#   scripts/slot-update.sh [a|b] [--switch]
#     a|b       target slot (default: the one not running)
#     --switch  then make it the boot slot (fw_setenv slot) and reboot
#
# Layout and slot logic: docs/emmc-layout.md (slot A = mmcblk0p2,
# slot B = mmcblk0p3, kernels boot-a.itb / boot-b.itb on mmcblk0p1).
set -eu

HERE=$(cd "$(dirname "$0")" && pwd)
CAM="$HERE/cam.sh"
I=$HERE/../output/sg2002_recamera_emmc/images
T=""; SWITCH=0
for a in "$@"; do
	case "$a" in
		a|b) T=$a ;;
		--switch) SWITCH=1 ;;
		*) echo "usage: slot-update.sh [a|b] [--switch]" >&2; exit 2 ;;
	esac
done

RUN=$(timeout 20 "$CAM" ssh "grep -o 'root=[^ ]*' /proc/cmdline")
case "$RUN" in
	root=/dev/mmcblk0p2) CUR=a ;;
	root=/dev/mmcblk0p3) CUR=b ;;
	*) echo "camera is not running from an eMMC slot ($RUN)" >&2; exit 1 ;;
esac
[ -n "$T" ] || { [ $CUR = a ] && T=b || T=a; }
[ "$T" != "$CUR" ] || { echo "slot $T is the one running" >&2; exit 1; }
[ "$T" = a ] && P=2 || P=3

R=$(readlink -f "$I/rootfs.ext4")
N=$(stat -c%s "$R")
SHA=$(sha256sum "$R" | cut -d' ' -f1)
KSHA=$(sha256sum "$I/boot.itb" | cut -d' ' -f1)
echo "slot $T (mmcblk0p$P), running $CUR; image $(cat "$I/VERSION" 2>/dev/null || cat "$I/VERSAO")"

timeout 300 "$CAM" ssh "grep -q mmcblk0p$P /proc/mounts && exit 1; dd of=/dev/mmcblk0p$P bs=4M conv=fsync 2>/dev/null" < "$R"
timeout 60 "$CAM" scp "$I/boot.itb" :/tmp/boot.itb
timeout 300 "$CAM" ssh "set -e; echo 3 > /proc/sys/vm/drop_caches
[ \$(head -c $N /dev/mmcblk0p$P | sha256sum | cut -d' ' -f1) = $SHA ] && echo rootfs_sha_ok
e2fsck -fy /dev/mmcblk0p$P >/dev/null 2>&1 || [ \$? -le 1 ]
resize2fs /dev/mmcblk0p$P >/dev/null 2>&1
tune2fs -L rootfs-$T -U random /dev/mmcblk0p$P >/dev/null
[ \$(sha256sum /tmp/boot.itb | cut -d' ' -f1) = $KSHA ]
mkdir -p /mnt/b; mount /dev/mmcblk0p1 /mnt/b
cp /tmp/boot.itb /mnt/b/boot-$T.itb.new; sync
[ \$(sha256sum /mnt/b/boot-$T.itb.new | cut -d' ' -f1) = $KSHA ]
mv /mnt/b/boot-$T.itb.new /mnt/b/boot-$T.itb; sync; umount /mnt/b; rm /tmp/boot.itb
echo kernel_${T}_ok"

if [ $SWITCH = 1 ]; then
	"$CAM" ssh "fw_setenv slot $T && sync && reboot" || true
	echo "rebooting into slot $T"
fi

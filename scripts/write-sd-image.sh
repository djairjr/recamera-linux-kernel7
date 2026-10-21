#!/usr/bin/env bash
# write-sd-image.sh — write a vendor SD-card image to a removable device, raw, verified.
#
# Why raw and never the files by hand: hand-copying the pack's files onto a card is
# exactly what already damaged the eMMC of this board. The vendor ships .img
# artifacts; writing the .img byte for byte is the same write balenaEtcher performs
# on them, which is what the official README prescribes.
#
# The vendor ships two card artifacts for this board, and they do different jobs
# (reCamera-OS README, section 2.4):
#
#   sg2002_reCamera_0.2.4_emmc_recovery.img
#       -> "SD Card recovery": balenaEtcher writes it to the card; insert the card
#          into the board and power on; the board burns the image into the eMMC.
#          600 MiB FAT partition (label RESERVED) carrying fip.bin, boot.emmc and
#          rootfs_ext4.emmc as files, straight out of the 0.2.4 eMMC pack.
#
#   sg2002_reCamera_0.2.4_emmc_sd_compat.img
#       -> "Booting from SD Card": the board runs the system from the card itself.
#          8 MiB FAT partition (flag boot, label BOOT) with fip.bin and boot.sd,
#          plus a 512 MiB ext4 rootfs partition.
#
# Safety: refuses anything that is not a removable device, refuses a mounted
# device, prints the card's own identity before writing anything, and verifies by
# reading the image back off the card and comparing digests.
#
# Also used for this project's own card: output/sg2002_recamera_emmc/images/sdcard.img.
#
# Usage:  pkexec bash "$PWD/scripts/write-sd-image.sh" "$PWD/<image.img>" <device>
#         (absolute paths: pkexec starts the command in /, a relative path
#         is not found -- the password is asked for nothing)
set -u

IMG=${1:-}
DEV=${2:-}

die() { echo "[ERR] $*" >&2; exit 1; }
say() { echo "[INFO] $*"; }

[ -n "$IMG" ] || die "usage: $0 <image.img> <device>"
[ -n "$DEV" ] || die "usage: $0 <image.img> <device>"
[ -f "$IMG" ] || die "no image: $IMG"
[ -b "$DEV" ] || die "$DEV is not a block device"

# /dev/sdX -> sdX ; /dev/mmcblk0 -> mmcblk0
NAME=$(basename "$(readlink -f "$DEV")")
SYS=/sys/block/$NAME
[ -d "$SYS" ] || die "$NAME is not in /sys/block -- not a whole disk?"
[ "$(cat "$SYS/removable" 2>/dev/null)" = "1" ] \
    || die "$NAME is not removable (removable=$(cat "$SYS/removable" 2>/dev/null)). Refused."

# Refuse anything that carries a mounted filesystem, the device or any partition of it.
if grep -qE "^/dev/${NAME}[0-9p]?[0-9]* " /proc/mounts; then
    echo "--- mounts found on $NAME ---" >&2
    grep -E "^/dev/${NAME}" /proc/mounts >&2
    die "unmount it first (udisksctl unmount -b ...). Nothing was written."
fi

IMG_BYTES=$(stat -c%s "$IMG")
DEV_BYTES=$(( $(cat "$SYS/size") * 512 ))
[ "$DEV_BYTES" -ge "$IMG_BYTES" ] \
    || die "card too small: $DEV_BYTES B < $IMG_BYTES B"

say "image    : $IMG"
say "           $IMG_BYTES bytes"
say "card     : $DEV"
say "           $DEV_BYTES bytes ($(( DEV_BYTES / 1024 / 1024 )) MiB)"
say "           model:  $(cat "$SYS/device/model" 2>/dev/null | xargs)"
say "           vendor: $(cat "$SYS/device/vendor" 2>/dev/null | xargs)"
say "           serial: $(cat "$SYS/device/serial" 2>/dev/null | xargs)"

say "image digest (the reference):"
IMG_MD5=$(md5sum "$IMG" | cut -d' ' -f1)
say "           md5 $IMG_MD5"

say "writing..."
dd if="$IMG" of="$DEV" bs=4M conv=fsync status=progress || die "dd failed"
sync

say "reading the card back to check ($IMG_BYTES bytes)..."
DEV_MD5=$(head -c "$IMG_BYTES" "$DEV" | md5sum | cut -d' ' -f1)
say "           md5 read from the card $DEV_MD5"

[ "$IMG_MD5" = "$DEV_MD5" ] || die "MISMATCH: card $DEV_MD5 != image $IMG_MD5"
say "CHECKED: the card is the image, byte for byte."

echo
case "$(basename "$IMG")" in
  *_sd_compat.img)
    echo "  SD BOOT artifact (README 2.4, 'Booting from SD Card'):"
    echo "  the card is a whole system -- fip + kernel (boot.sd) + ext4 rootfs."
    echo "  With the camera off, insert the card and power it on."
    ;;
  *_recovery.img)
    echo "  RECOVERY artifact (README 2.4, 'SD Card recovery'):"
    echo "  with the camera off, insert the card and power it on;"
    echo "  it writes the image into the eMMC by itself."
    ;;
  sdcard.img)
    echo "  This project's card: with the camera off, insert it and power on; it runs"
    echo "  from the card. To install into the eMMC, log in and run: emmc-install"
    ;;
  *)
    echo "  Image written and checked."
    ;;
esac

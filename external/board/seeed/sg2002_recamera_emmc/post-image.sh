#!/bin/sh
# SPDX-License-Identifier: GPL-2.0
#
# Buildroot post-image script (manual, 9.7) for the Seeed reCamera (SG2002).
#
# It produces, in BINARIES_DIR (images/):
#   boot.sd / boot.emmc  the kernel FIT (Image.gz + DTB), under both names:
#                        "boot.sd" is what the U-Boot loads from the SD card
#                        (`fatload mmc 1 ${uImage_addr} boot.sd'), "boot.emmc"
#                        is what goes into the eMMC BOOT partition
#   fip.bin              the FSBL + OpenSBI + U-Boot the ROM loads
#   sdcard.img           the card image built by genimage: p1 FAT with fip.bin +
#                        boot.sd, p2 ext4 with the rootfs, p3 FAT "INSTALL" with
#                        the eMMC installation set (see below)
#
# Every name and step below comes from measurement, not assumption:
#  - the FIT configuration is named config-sg2002_recamera_emmc because the
#    vendor U-Boot boots with `bootm ${uImage_addr}#config-<FDT_NO>', where
#    FDT_NO = __stringify(CVICHIP) "_" __stringify(CVIBOARD)
#    (vendor/reCamera-OS/external/u-boot/include/configs/cv181x-asic.h:200,307);
#    CHIP=sg2002 and BOARD=recamera_emmc come from the META/misc_info.txt of the
#    official package.
#  - the kernel goes compressed (Image.gz, "gzip"), as in the `boot' target of
#    the reCamera-OS build/Makefile (KERNEL_COMPRESS, KERNEL_GZIP by default):
#    the BOOT partition is 8192 KB (partition_emmc.xml) while the raw Image is
#    ~9 MB.
#  - the load/entry address 0x80200000 is the CONFIG_KERNEL_ENTRY_HACK_ADDR of
#    the SDK.
#  - fip.bin is installed into BINARIES_DIR by the recamera-fip package: the
#    official one (package/recamera-fip/fip-seeed-original.bin, md5
#    384dd24fe63da69b5c7bcee77c75e75a, from ~/recamera/oficial/emmc-usbdl and
#    identical to the FIP.BIN of the card release) with only its OpenSBI
#    replaced by the same vendor OpenSBI 0.9 plus the fence.tso emulation the
#    C906 needs; make-fip.sh there rebuilds and checks it (F7 step 9).
#
# mkimage comes from host-uboot-tools; /incbin/ paths in the .its are relative to
# the .its location, so the recipe is copied next to the images and mkimage runs
# from there.

set -eu

BOARD_DIR="$(dirname "$0")"
BINARIES_DIR="${BINARIES_DIR:-$(pwd)}"
MKIMAGE="${HOST_DIR}/bin/mkimage"

if [ ! -x "${MKIMAGE}" ]; then
	echo "post-image: ${MKIMAGE} not found (enable BR2_PACKAGE_HOST_UBOOT_TOOLS)" >&2
	exit 1
fi

cp -f "${BOARD_DIR}/boot.its" "${BINARIES_DIR}/boot.its"

gzip -9 -f -k "${BINARIES_DIR}/Image"

(
	cd "${BINARIES_DIR}"
	"${MKIMAGE}" -f boot.its boot.sd
)

cp -f "${BINARIES_DIR}/boot.sd" "${BINARIES_DIR}/boot.emmc"

if [ ! -s "${BINARIES_DIR}/fip.bin" ]; then
	echo "post-image: ${BINARIES_DIR}/fip.bin missing (enable BR2_PACKAGE_RECAMERA_FIP)" >&2
	exit 1
fi

# eMMC installation set (SD card p3 INSTALL, consumed by /usr/sbin/emmc-install):
#   boot.itb       the same FIT, installed as boot-a.itb / boot-b.itb
#   uboot-env.bin  the U-Boot environment of the eMMC: 128 KiB, single copy,
#                  CRC32 + data (the format the official fip's U-Boot saved
#                  at 8 MiB, measured on 18/10/2026)
#   SHA256SUMS     checked by emmc-install before anything is written
cp -f "${BINARIES_DIR}/boot.sd" "${BINARIES_DIR}/boot.itb"
"${HOST_DIR}/bin/mkenvimage" -s 0x20000 -o "${BINARIES_DIR}/uboot-env.bin" \
	"${BOARD_DIR}/uboot-env-emmc.txt"
# the same line the rootfs carries (post-build.sh): card and slots agree
cp -f "${TARGET_DIR}/etc/recamera-version" "${BINARIES_DIR}/VERSION"
(
	cd "${BINARIES_DIR}"
	sha256sum fip.bin boot.itb rootfs.ext4 uboot-env.bin VERSION > SHA256SUMS
)

# The card image: the Buildroot genimage with the layout of genimage.cfg. It
# runs from the Buildroot directory, with BINARIES_DIR and HOST_DIR exported.
support/scripts/genimage.sh -c "${BOARD_DIR}/genimage.cfg"

echo "post-image: boot.sd $(stat -c%s "${BINARIES_DIR}/boot.sd") bytes," \
	"sdcard.img $(stat -c%s "${BINARIES_DIR}/sdcard.img") bytes"

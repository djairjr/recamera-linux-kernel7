################################################################################
#
# recamera-fip -- the board fip.bin (FSBL + OpenSBI + U-Boot + the second
# core's FreeRTOS), the one the camera boots: the official Seeed fip with
# only its OpenSBI replaced (vendor OpenSBI 0.9 + fence.tso emulation)
#
################################################################################

RECAMERA_FIP_VERSION = 1
RECAMERA_FIP_SITE = $(BR2_EXTERNAL_RECAMERA_PATH)/package/recamera-fip
RECAMERA_FIP_SITE_METHOD = local
RECAMERA_FIP_LICENSE = BSD-3-Clause (FSBL), BSD-2-Clause (OpenSBI), GPL-2.0+ (U-Boot), MIT (FreeRTOS)
RECAMERA_FIP_LICENSE_FILES = README.md
RECAMERA_FIP_INSTALL_IMAGES = YES
RECAMERA_FIP_INSTALL_TARGET = NO

# Prebuilt and checked in (fip.bin): rebuilding it needs the vendor SDK
# (T-Head toolchain, fiptool.py), which make-fip.sh uses; see README.md.
define RECAMERA_FIP_INSTALL_IMAGES_CMDS
	cd $(@D) && echo "$(RECAMERA_FIP_SHA256)  fip.bin" | sha256sum -c -
	$(INSTALL) -D -m 0644 $(@D)/fip.bin $(BINARIES_DIR)/fip.bin
endef

RECAMERA_FIP_SHA256 = 2db62808d2d63216ffb672ca4b64e8b456a0e709d8c7ba206a8654fcf8efadaf

$(eval $(generic-package))

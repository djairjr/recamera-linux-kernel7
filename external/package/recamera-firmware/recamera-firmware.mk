################################################################################
#
# recamera-firmware -- the firmware files of the reCamera radios, from their
# public sources at pinned commits (no binary in this tree)
#
################################################################################

RECAMERA_FIRMWARE_VERSION = 1
RECAMERA_FIRMWARE_SOURCE =
RECAMERA_FIRMWARE_LICENSE = Cypress/Infineon (CYW43012 firmware), Morse Micro (MM6108 firmware, see WHENCE.md)

# CYW43012 Wi-Fi: the files of the vendor image (reCamera-OS 0.2.4 overlay)
RECAMERA_FIRMWARE_SEEED_OS = https://raw.githubusercontent.com/Seeed-Studio/reCamera-OS/01debc9967752448e29a745f37d29d80a4a41e37/external/buildroot/board/cvitek/CV181X/overlay/lib/firmware/brcm
# CYW43012 Bluetooth patch RAM: Seeed's brcm-patchram-plus package
RECAMERA_FIRMWARE_SEEED_BT = https://raw.githubusercontent.com/Seeed-Studio/sscma-example-sg200x/c705d180571e11ff41cd56098c0d95734daff521/solutions/brcm-patchram-plus/rootfs/usr/lib/firmware/brcm
# MM6108 HaLow: Morse Micro's release, byte-identical to the vendor image's
RECAMERA_FIRMWARE_MORSE = https://raw.githubusercontent.com/MorseMicro/morse-firmware/0109315ac66cd024651c38a00c94c84e4a59779f

RECAMERA_FIRMWARE_EXTRA_DOWNLOADS = \
	$(RECAMERA_FIRMWARE_SEEED_OS)/brcmfmac43012-sdio.bin \
	$(RECAMERA_FIRMWARE_SEEED_OS)/brcmfmac43012-sdio.clm_blob \
	$(RECAMERA_FIRMWARE_SEEED_OS)/brcmfmac43012-sdio.txt \
	$(RECAMERA_FIRMWARE_SEEED_BT)/CYW43012C0_003.001.015.0196.0269.hcd \
	$(RECAMERA_FIRMWARE_MORSE)/firmware/mm6108.bin \
	$(RECAMERA_FIRMWARE_MORSE)/bcf/quectel/bcf_fgh100mhaamd.bin

# brcmfmac looks for the NVRAM file under the board compatible first: both
# names of this board point at the one file. The Morse driver loads
# bcf_default.bin unless told otherwise: the FGH100M module's BCF.
define RECAMERA_FIRMWARE_INSTALL_TARGET_CMDS
	$(INSTALL) -d $(TARGET_DIR)/lib/firmware/brcm $(TARGET_DIR)/lib/firmware/morse
	$(foreach f,brcmfmac43012-sdio.bin brcmfmac43012-sdio.clm_blob brcmfmac43012-sdio.txt \
		CYW43012C0_003.001.015.0196.0269.hcd,\
		$(INSTALL) -m 0644 $(RECAMERA_FIRMWARE_DL_DIR)/$(f) $(TARGET_DIR)/lib/firmware/brcm/$(f)$(sep))
	ln -sf brcmfmac43012-sdio.txt "$(TARGET_DIR)/lib/firmware/brcm/brcmfmac43012-sdio.cvitek,cv181x.txt"
	ln -sf brcmfmac43012-sdio.txt "$(TARGET_DIR)/lib/firmware/brcm/brcmfmac43012-sdio.seeed,recamera.txt"
	$(INSTALL) -m 0644 $(RECAMERA_FIRMWARE_DL_DIR)/mm6108.bin $(TARGET_DIR)/lib/firmware/morse/mm6108.bin
	$(INSTALL) -m 0644 $(RECAMERA_FIRMWARE_DL_DIR)/bcf_fgh100mhaamd.bin $(TARGET_DIR)/lib/firmware/morse/bcf_default.bin
endef

$(eval $(generic-package))

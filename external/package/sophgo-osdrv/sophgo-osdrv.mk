################################################################################
#
# sophgo-osdrv -- the Sophgo multimedia kernel modules (vendor osdrv/interdrv,
# cv181x), built out of tree for Linux 7.2
#
################################################################################

# sophgo/osdrv, the commit of the reCamera-OS 0.2.4 submodule; the drivers
# are its interdrv/ directory
SOPHGO_OSDRV_VERSION = f35ca075cc022aac97f5d22238c1e46caf8276bd
SOPHGO_OSDRV_SITE = $(call github,sophgo,osdrv,$(SOPHGO_OSDRV_VERSION))
SOPHGO_OSDRV_LICENSE = GPL-2.0
SOPHGO_OSDRV_DEPENDENCIES = sophgo-ion

# ION headers (ion/ion.h, ion/cvitek/...) and symbols (cvi_ion_alloc...)
SOPHGO_OSDRV_MODULE_MAKE_OPTS = \
	SOPHGO_ION_DIR=$(SOPHGO_ION_DIR) \
	KBUILD_EXTRA_SYMBOLS=$(SOPHGO_ION_DIR)/Module.symvers

# The package is interdrv/: move it up before the patches (paths relative to
# interdrv/, as port.sh makes them)
define SOPHGO_OSDRV_KEEP_INTERDRV
	mv $(@D)/interdrv $(BUILD_DIR)/sophgo-osdrv-interdrv.tmp
	rm -rf $(@D)
	mv $(BUILD_DIR)/sophgo-osdrv-interdrv.tmp $(@D)
endef
SOPHGO_OSDRV_POST_EXTRACT_HOOKS += SOPHGO_OSDRV_KEEP_INTERDRV

# The out-of-tree Kbuild in place of the vendor Makefiles
define SOPHGO_OSDRV_ADD_KBUILD
	cp $(SOPHGO_OSDRV_PKGDIR)/Kbuild $(@D)/Kbuild
	cp -r $(SOPHGO_OSDRV_PKGDIR)/kernel-5.10-headers $(@D)/kernel-5.10-headers
	rm -f $(@D)/Makefile
endef
SOPHGO_OSDRV_POST_PATCH_HOOKS += SOPHGO_OSDRV_ADD_KBUILD

# Video codec MCU firmware (Chips&Media, prebuilt by Sophgo; the same blob
# for every CPU arch: it runs on the codec's own controller). cvi_vc_driver
# reads it from this path when the first H.264 (coda980) / H.265 (monet)
# channel is created. From sophgo/ramdisk, the reCamera-OS 0.2.4 commit.
SOPHGO_OSDRV_FW = https://raw.githubusercontent.com/sophgo/ramdisk/f855ea377c886aae094e224f4dffebcffaedc4ad/rootfs/common_musl_riscv64/usr/share/fw_vcodec
SOPHGO_OSDRV_EXTRA_DOWNLOADS = $(SOPHGO_OSDRV_FW)/coda980.bin $(SOPHGO_OSDRV_FW)/monet.bin
define SOPHGO_OSDRV_INSTALL_FW
	$(INSTALL) -D -m 0644 $(SOPHGO_OSDRV_DL_DIR)/coda980.bin $(TARGET_DIR)/usr/share/fw_vcodec/coda980.bin
	$(INSTALL) -D -m 0644 $(SOPHGO_OSDRV_DL_DIR)/monet.bin $(TARGET_DIR)/usr/share/fw_vcodec/monet.bin
endef
SOPHGO_OSDRV_POST_INSTALL_TARGET_HOOKS += SOPHGO_OSDRV_INSTALL_FW

$(eval $(kernel-module))
$(eval $(generic-package))

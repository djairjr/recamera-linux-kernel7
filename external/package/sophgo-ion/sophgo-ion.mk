################################################################################
#
# sophgo-ion -- the Sophgo/Cvitek ION memory allocator (vendor 5.10 staging
# ION), built out of tree for Linux 7.2: cvi_ion.ko, /dev/ion
#
################################################################################

SOPHGO_ION_VERSION = sg2002-5.10
# The vendor files, unmodified (src/README.md: sophgo/linux_5.10 at
# d0486a96, drivers/staging/android): 10 files instead of the whole kernel.
SOPHGO_ION_SITE = $(BR2_EXTERNAL_RECAMERA_PATH)/package/sophgo-ion/src
SOPHGO_ION_SITE_METHOD = local
SOPHGO_ION_LICENSE = GPL-2.0

# Local sources skip package patches: apply the 7.2 port patch and drop in the
# out-of-tree Kbuild right after the rsync (src/ stays as the vendor's).
define SOPHGO_ION_APPLY_PORT
	$(APPLY_PATCHES) $(@D) $(SOPHGO_ION_PKGDIR) \*.patch
	cp $(SOPHGO_ION_PKGDIR)/Kbuild $(@D)/Kbuild
endef
SOPHGO_ION_POST_RSYNC_HOOKS += SOPHGO_ION_APPLY_PORT

# Headers for the other Sophgo modules (sys, tpu, vcodec) and for userspace.
define SOPHGO_ION_INSTALL_STAGING_CMDS
	$(INSTALL) -d $(STAGING_DIR)/usr/include/sophgo-ion/uapi
	$(INSTALL) -m 0644 $(@D)/ion/ion.h $(@D)/ion/cvitek/cvitek_ion_alloc.h \
		$(STAGING_DIR)/usr/include/sophgo-ion/
	$(INSTALL) -m 0644 $(@D)/uapi/ion.h $(@D)/uapi/ion_cvitek.h \
		$(STAGING_DIR)/usr/include/sophgo-ion/uapi/
endef
SOPHGO_ION_INSTALL_STAGING = YES

$(eval $(kernel-module))
$(eval $(generic-package))

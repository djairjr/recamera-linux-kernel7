################################################################################
#
# wpa-supplicant-s1g -- Morse Micro hostap for HaLow (upstream release
# 1.14.1, the release of the driver)
#
################################################################################

WPA_SUPPLICANT_S1G_VERSION = 1.14.1
WPA_SUPPLICANT_S1G_SITE = $(call github,MorseMicro,hostap,$(WPA_SUPPLICANT_S1G_VERSION))
WPA_SUPPLICANT_S1G_LICENSE = BSD-3-Clause
WPA_SUPPLICANT_S1G_LICENSE_FILES = README
WPA_SUPPLICANT_S1G_DEPENDENCIES = host-pkgconf libnl openssl
WPA_SUPPLICANT_S1G_CONFIG = $(@D)/wpa_supplicant/.config
# hostap predates C23; its -Werror meets GCC 15 on the 2-letter country codes
# kept in char[2] on purpose (no NUL), and OpenSSL 3 on the EC_KEY calls of
# DPP (deprecated, still there)
WPA_SUPPLICANT_S1G_CFLAGS = $(TARGET_CFLAGS) -std=gnu17 -Wno-unterminated-string-initialization \
	-Wno-error=deprecated-declarations \
	-I$(STAGING_DIR)/usr/include/libnl3/
WPA_SUPPLICANT_S1G_LIBS = -lnl-3 -lm -lpthread

# Vendor recipe: defconfig + EAP-FAST.
define WPA_SUPPLICANT_S1G_CONFIGURE_CMDS
	cp $(@D)/wpa_supplicant/defconfig $(WPA_SUPPLICANT_S1G_CONFIG)
	sed -i -e 's/^#\(CONFIG_EAP_FAST\)/\1/' $(WPA_SUPPLICANT_S1G_CONFIG)
endef

define WPA_SUPPLICANT_S1G_BUILD_CMDS
	$(TARGET_MAKE_ENV) PKG_CONFIG_SYSROOT_DIR="$(STAGING_DIR)" \
		PKG_CONFIG_PATH="$(STAGING_DIR)/usr/lib/pkgconfig" \
		CFLAGS="$(WPA_SUPPLICANT_S1G_CFLAGS)" LDFLAGS="$(TARGET_LDFLAGS)" \
		LIBS="$(WPA_SUPPLICANT_S1G_LIBS)" LIBS_c="$(WPA_SUPPLICANT_S1G_LIBS)" \
		LIBS_p="$(WPA_SUPPLICANT_S1G_LIBS)" \
		$(MAKE) CC="$(TARGET_CC)" -C $(@D)/wpa_supplicant
endef

define WPA_SUPPLICANT_S1G_INSTALL_TARGET_CMDS
	$(INSTALL) -m 0755 -D $(@D)/wpa_supplicant/wpa_supplicant_s1g $(TARGET_DIR)/usr/sbin/wpa_supplicant_s1g
	$(INSTALL) -m 0755 -D $(@D)/wpa_supplicant/wpa_cli_s1g $(TARGET_DIR)/usr/sbin/wpa_cli_s1g
endef

$(eval $(generic-package))

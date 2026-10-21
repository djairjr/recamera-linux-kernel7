################################################################################
#
# morse-cli -- Morse Micro control tool (upstream release 1.14.1, the
# release of the driver)
#
################################################################################

MORSE_CLI_VERSION = 1.14.1
MORSE_CLI_SITE = $(call github,MorseMicro,morse_cli,$(MORSE_CLI_VERSION))
MORSE_CLI_LICENSE = GPL-2.0+ (see LICENSING)
MORSE_CLI_LICENSE_FILES = LICENSING
# usb.c is always built since 1.14 (USB-attached chips): libusb is needed
# even though the reCamera chip is on SDIO
MORSE_CLI_DEPENDENCIES = host-pkgconf libnl libusb
# The code predates C23 (e.g. "void usage()" called with arguments).
MORSE_CLI_CFLAGS = $(TARGET_CFLAGS) -std=gnu17 -I$(STAGING_DIR)/usr/include/libnl3/

define MORSE_CLI_BUILD_CMDS
	$(TARGET_MAKE_ENV) CONFIG_MORSE_TRANS_NL80211=1 PKG_CONFIG="$(PKG_CONFIG_HOST_BINARY)" \
		CFLAGS="$(MORSE_CLI_CFLAGS)" LDFLAGS="$(TARGET_LDFLAGS)" \
		$(MAKE) CC="$(TARGET_CC)" -C $(@D)
endef

define MORSE_CLI_INSTALL_TARGET_CMDS
	$(INSTALL) -m 0755 -D $(@D)/morse_cli $(TARGET_DIR)/usr/sbin/morse_cli
endef

$(eval $(generic-package))

################################################################################
#
# recamera-tests -- the port's check programs (tests/ of this tree)
#
################################################################################

RECAMERA_TESTS_VERSION = 1
RECAMERA_TESTS_SITE = $(BR2_EXTERNAL_RECAMERA_PATH)/../tests
RECAMERA_TESTS_SITE_METHOD = local
RECAMERA_TESTS_LICENSE = GPL-2.0

# <installed name>:<source under testes/>
RECAMERA_TESTS_PROGS = \
	recamera-iontest:f7/iontest.c \
	recamera-ioninfo:f7/ioninfo.c \
	recamera-fencetso:f7/fencetso.c

define RECAMERA_TESTS_BUILD_CMDS
	$(foreach p,$(RECAMERA_TESTS_PROGS),\
		$(TARGET_CC) $(TARGET_CFLAGS) $(TARGET_LDFLAGS) -Wall -Werror \
			-o $(@D)/$(firstword $(subst :, ,$(p))) $(@D)/$(lastword $(subst :, ,$(p))) || exit 1;)
endef

define RECAMERA_TESTS_INSTALL_TARGET_CMDS
	$(foreach p,$(RECAMERA_TESTS_PROGS),\
		$(INSTALL) -D -m 0755 $(@D)/$(firstword $(subst :, ,$(p))) \
			$(TARGET_DIR)/usr/bin/$(firstword $(subst :, ,$(p)));)
endef

$(eval $(generic-package))

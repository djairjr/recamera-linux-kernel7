################################################################################
#
# morse-driver -- Morse Micro HaLow driver (upstream release 1.14.1, the one
# the vendor image shipped), ported to 7.2
#
################################################################################

MORSE_DRIVER_VERSION = 1.14.1
MORSE_DRIVER_SITE = https://github.com/MorseMicro/morse_driver.git
MORSE_DRIVER_SITE_METHOD = git
# mmrc-submodule: the rate control (MorseMicro/mm_rate_control)
MORSE_DRIVER_GIT_SUBMODULES = YES
MORSE_DRIVER_LICENSE = GPL-2.0+ (module), Morse Micro Source Code License Agreement (parts)
MORSE_DRIVER_LICENSE_FILES = LICENSE

# Vendor defconfig (cvitek_sg2002_recamera_emmc_defconfig:96-100).
MORSE_DRIVER_MODULE_MAKE_OPTS = \
	KERNEL_SRC=$(LINUX_DIR) \
	CONFIG_WLAN_VENDOR_MORSE=m \
	CONFIG_MORSE_SDIO=y \
	CONFIG_MORSE_SDIO_ALIGNMENT=2 \
	CONFIG_MORSE_USER_ACCESS=y \
	CONFIG_MORSE_VENDOR_COMMAND=y \
	DEBUG=n

$(eval $(kernel-module))
$(eval $(generic-package))

# SPDX-License-Identifier: GPL-2.0
#
# Top makefile of the reCamera (SG2002) Buildroot project.
#
# Recipe: Buildroot manual, "Custom top Makefile"
# (buildroot/docs/manual/customize-directory-structure.adoc), adapted to this layout:
#
#   <here>/external    br2-external tree (external.desc, Config.in, external.mk,
#                      configs/, board/, package/, patches/)
#   <here>/buildroot   Buildroot clone, 2026.05.3
#   <here>/dl          shared download cache
#   <here>/output/     one output directory per defconfig
#
# Entry points:
#   make sg2002_recamera_emmc_defconfig
#       reads external/configs/<board>_defconfig and configures into output/<board>
#   make <any other goal>
#       runs that Buildroot goal (busybox, linux-menuconfig, savedefconfig,
#       busybox-update-config, linux-update-defconfig, ...) in the single output
#       directory of this project, with the same BR2_EXTERNAL and BR2_DL_DIR
#
# Run every one of them inside the build image (container/run.sh): that image is
# where this project's toolchain lives, so the same goals on the host fail on purpose.

# Avoid surprises from default suffix rules in this Makefile. Not through
# MAKEFLAGS += --no-builtin-rules: MAKEFLAGS is inherited by every sub-make,
# so it reached the package Makefiles that rely on make's built-in %.o: %.c
# (host-e2fsprogs, host-xxhash: "No rule to make target 'xxx.o'", 19/10/2026).
.SUFFIXES:

THIS_EXTERNAL_PATH := $(abspath $(dir $(lastword $(MAKEFILE_LIST))))
EXTERNAL_DIR := $(THIS_EXTERNAL_PATH)/external

# The single board of this project: its defconfig names the output directory.
BOARDNAME := sg2002_recamera_emmc

# Keep downloads shared inside this tree instead of in the Buildroot directory.
# It must be exported: Buildroot reads BR2_DL_DIR from the environment, and its
# own Makefile states that the environment variable overrides the .config option.
ifeq ($(BR2_DL_DIR),)
BR2_DL_DIR = $(THIS_EXTERNAL_PATH)/dl
endif
export BR2_DL_DIR

OUTPUT_BASEDIR = $(THIS_EXTERNAL_PATH)/output
OUTPUT_DIR = $(OUTPUT_BASEDIR)/$(patsubst %_defconfig,%,$@)

MAKE_BUILDROOT = $(MAKE) -C $(THIS_EXTERNAL_PATH)/buildroot BR2_EXTERNAL=$(EXTERNAL_DIR)

# Configuring the board. This rule comes first so that it wins for a target that
# names an existing file under external/configs/.
%: $(EXTERNAL_DIR)/configs/%
	$(MAKE_BUILDROOT) O=$(OUTPUT_DIR) $@

# Any other goal runs in the one output directory of this project.
%:
	$(MAKE_BUILDROOT) O=$(OUTPUT_BASEDIR)/$(BOARDNAME) $@

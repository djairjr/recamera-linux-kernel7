################################################################################
#
# sophgo-mpi -- the Sophgo multimedia userspace (vendor cvi_mpi, cv181x):
# libsys, libvi, libisp + the closed 3A objects (musl build), libcvi_bin
#
################################################################################

# sophgo/cvi_mpi, the commit of the reCamera-OS 0.2.4 submodule
SOPHGO_MPI_COMMIT = 6dbbbb8bed877cd3a08f76ce4aed61645c38bbe1
SOPHGO_MPI_VERSION = $(SOPHGO_MPI_COMMIT)
SOPHGO_MPI_SITE = $(call github,sophgo,cvi_mpi,$(SOPHGO_MPI_COMMIT))
SOPHGO_MPI_LICENSE = Sophgo MPI (source), proprietary prebuilt ISP 3A objects
SOPHGO_MPI_INSTALL_STAGING = YES

# The vendor Makefiles, with:
#  TARGET_MACHINE  the vendor triplet, so the prebuilt objects of the closed
#                  ISP algorithms are picked in their musl flavour
#                  (*.riscv64-unknown-linux-musl.cv181x.o)
#  OPT_LEVEL       our GCC: the vendor flags (-mcpu=c906fdv
#                  -march=rv64imafdcv0p7xthead) belong to the T-Head
#                  toolchain. Our code is plain rv64gc; the prebuilt objects
#                  keep their v0.7/xthead code (the 7.2 kernel enables
#                  xtheadvector for userspace, /proc/cpuinfo).
SOPHGO_MPI_MAKE_OPTS = \
	CROSS_COMPILE=$(TARGET_CROSS) \
	TARGET_MACHINE=riscv64-unknown-linux-musl \
	CHIP_ARCH=CV181X \
	OPT_LEVEL="-Os"
# (no PARAM_FILE here: a vendor Makefile includes mpi_param.mk only when
# PARAM_FILE is empty, finding it by its relative path)

# The sensor drivers (vendor SensorSupportList) build from inside the MPI tree
# (component/isp/..., their Makefiles find mpi_param.mk by relative path), as
# the vendor build lays them out. Only the OV5647 of the reCamera is taken.
# sophgo/SensorSupportList, the reCamera-OS 0.2.4 commit; copied in before
# the patches, some of which touch the OV5647 files
SOPHGO_MPI_SENSOR_COMMIT = 86b0708e0101dfd4b60abafe98fef28c92773dbb
SOPHGO_MPI_EXTRA_DOWNLOADS = \
	https://github.com/sophgo/SensorSupportList/archive/$(SOPHGO_MPI_SENSOR_COMMIT).tar.gz
define SOPHGO_MPI_ADD_SENSOR
	mkdir -p $(@D)/sensor-src $(@D)/component/isp/sensor/cv181x
	tar -xzf $(SOPHGO_MPI_DL_DIR)/$(SOPHGO_MPI_SENSOR_COMMIT).tar.gz --strip-components=1 -C $(@D)/sensor-src
	cp -a $(@D)/sensor-src/common $(@D)/component/isp/
	cp -a $(@D)/sensor-src/sensor/cv181x/ov_ov5647 $(@D)/component/isp/sensor/cv181x/
	rm -rf $(@D)/sensor-src
endef
SOPHGO_MPI_POST_EXTRACT_HOOKS += SOPHGO_MPI_ADD_SENSOR


# vpss: needed by step 6 and by libcvi_bin (CVI_BIN_ImportBinData walks the
# VPSS/VO sections of the tuning bin); vo: library only, for libcvi_bin --
# no display, its kernel driver is not built.
SOPHGO_MPI_MODULES = sys bin vi vpss vo venc
SOPHGO_MPI_ISP_PARTS = isp_algo ../algo isp isp_bin

define SOPHGO_MPI_BUILD_CMDS
	sed -i '19,21c\#ifndef __CV181X__\n\t#define __CV181X__\n#endif' $(@D)/include/linux/cvi_defines.h
	mkdir -p $(@D)/lib/3rd
	$(foreach m,$(SOPHGO_MPI_MODULES),\
		$(TARGET_MAKE_ENV) $(MAKE) -C $(@D)/modules/$(m) $(SOPHGO_MPI_MAKE_OPTS) all || exit 1;)
	$(foreach m,$(SOPHGO_MPI_ISP_PARTS),\
		$(TARGET_MAKE_ENV) $(MAKE) -C $(@D)/modules/isp/cv181x/$(m) $(SOPHGO_MPI_MAKE_OPTS) all || exit 1;)
	$(TARGET_MAKE_ENV) $(MAKE) -C $(@D)/component/isp/sensor/cv181x/ov_ov5647 $(SOPHGO_MPI_MAKE_OPTS) all
	$(SOPHGO_MPI_BUILD_CAM)
	$(SOPHGO_MPI_BUILD_VICAP)
endef

# librecamera-cam.a: the camera bring-up (recamera-cam.h) with the vendor
# sample common code it is built on; linked by recamera-vicap and, through
# app/sophgo-sys, by recamerad. -Wno-stringop-truncation: for the vendor
# 3rdparty inih only.
SOPHGO_MPI_SAMPLE = $(@D)/sample/common
SOPHGO_MPI_CAM_SRCS = recamera-cam.c sample_common_sys.c sample_common_vi.c \
	sample_common_isp.c sample_common_bin.c sample_common_sensor.c ini.c sensor_list.c
SOPHGO_MPI_CAM_CFLAGS = $(TARGET_CFLAGS) -Wall -Wno-stringop-truncation \
	-D__CV181X__ -DOS_IS_LINUX -DSENSOR_OV_OV5647 \
	-I$(@D)/include -I$(@D)/include/isp/cv181x -I$(SOPHGO_MPI_SAMPLE) \
	-I$(@D)/3rdparty/inih -I$(@D)/modules/sys/include -I$(@D)/modules/vi/include \
	-I$(@D)/modules/bin/include -I$(@D)/component/isp/common -I$(SOPHGO_MPI_PKGDIR)
# the libraries a program linking librecamera-cam.a needs (also in
# app/sophgo-sys/build.rs)
SOPHGO_MPI_CAM_LIBS = -Wl,--start-group -lrecamera-cam -lsys -lvi -lvpss -lvo -lvenc \
	-lisp -lisp_algo -lae -lawb -laf -lcvi_bin -lcvi_bin_isp -lsns_ov5647 \
	-Wl,--end-group -lpthread -ldl -lm -latomic

define SOPHGO_MPI_BUILD_CAM
	mkdir -p $(@D)/recamera-cam
	cp $(SOPHGO_MPI_PKGDIR)/recamera-cam.c $(SOPHGO_MPI_PKGDIR)/recamera-cam.h \
		$(SOPHGO_MPI_SAMPLE)/sample_common_*.c $(@D)/3rdparty/inih/ini.c \
		$(@D)/component/isp/common/sensor_list.c $(@D)/recamera-cam/
	cd $(@D)/recamera-cam && \
		$(TARGET_CC) $(SOPHGO_MPI_CAM_CFLAGS) -c $(SOPHGO_MPI_CAM_SRCS) && \
		rm -f $(@D)/lib/librecamera-cam.a && \
		$(TARGET_AR) rcs $(@D)/lib/librecamera-cam.a $(SOPHGO_MPI_CAM_SRCS:.c=.o)
endef

# recamera-vicap: capture/encode/YOLO check of the camera pipeline (F7).
define SOPHGO_MPI_BUILD_VICAP
	$(TARGET_CC) $(SOPHGO_MPI_CAM_CFLAGS) \
		-o $(@D)/recamera-vicap $(SOPHGO_MPI_PKGDIR)/recamera-vicap.c \
		-L$(@D)/lib $(SOPHGO_MPI_VICAP_NPU) $(SOPHGO_MPI_CAM_LIBS)
endef

# recamera-vicap "yolo" mode (F7 step 9) when the NPU runtime is built
ifeq ($(BR2_PACKAGE_SOPHGO_CVIRUNTIME),y)
SOPHGO_MPI_DEPENDENCIES += sophgo-cviruntime
SOPHGO_MPI_VICAP_NPU = -DRECAMERA_NPU -I$(STAGING_DIR)/usr/include/sophgo-tpu \
	$(SOPHGO_MPI_PKGDIR)/recamera-yolo.c -lcviruntime -lcvikernel
endif

define SOPHGO_MPI_INSTALL_STAGING_CMDS
	$(INSTALL) -d $(STAGING_DIR)/usr/include/sophgo-mpi
	cp -a $(@D)/include/. $(STAGING_DIR)/usr/include/sophgo-mpi/
	$(INSTALL) -m 0644 $(SOPHGO_MPI_PKGDIR)/recamera-cam.h $(STAGING_DIR)/usr/include/sophgo-mpi/
	cp -a $(@D)/lib/*.so $(@D)/lib/*.a $(STAGING_DIR)/usr/lib/
endef

# ISP tuning of the reCamera OV5647: the only one there is is Seeed's
# (owner's decision of 18/10/2026, calibration data, not software), installed
# where libcvi_bin looks for it (cvi_bin.c: /mnt/cfg/param/cvi_sdr_bin).
SOPHGO_MPI_EXTRA_DOWNLOADS += \
	https://raw.githubusercontent.com/Seeed-Studio/reCamera-OS/01debc9967752448e29a745f37d29d80a4a41e37/external/isp_tuning/cv181x/src/ov_ov5647/ov_ov5647_sdr.bin
SOPHGO_MPI_TUNING = $(SOPHGO_MPI_DL_DIR)/ov_ov5647_sdr.bin

define SOPHGO_MPI_INSTALL_TARGET_CMDS
	cp -a $(@D)/lib/*.so $(TARGET_DIR)/usr/lib/
	$(INSTALL) -D -m 0644 $(SOPHGO_MPI_TUNING) $(TARGET_DIR)/mnt/cfg/param/cvi_sdr_bin
	$(INSTALL) -D -m 0755 $(@D)/recamera-vicap $(TARGET_DIR)/usr/bin/recamera-vicap
endef

$(eval $(generic-package))

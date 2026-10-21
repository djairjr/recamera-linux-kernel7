################################################################################
#
# sophgo-cviruntime -- the Sophgo NPU userspace (vendor cviruntime + cvikernel
# + cvibuilder, cv181x), built for musl
#
################################################################################

# sophgo/cviruntime and its sibling repositories, at the commits of the
# reCamera-OS 0.2.4 submodules
SOPHGO_CVIRUNTIME_VERSION = 3f4938648950a7f3bf9a19c320ca9fae7c52de20
SOPHGO_CVIRUNTIME_SITE = $(call github,sophgo,cviruntime,$(SOPHGO_CVIRUNTIME_VERSION))
SOPHGO_CVIRUNTIME_LICENSE = BSD-2-Clause (cviruntime, cvikernel), Apache-2.0 (flatbuffers)
SOPHGO_CVIRUNTIME_INSTALL_STAGING = YES
SOPHGO_CVIRUNTIME_DEPENDENCIES = host-cmake

# The sibling SDK parts cviruntime builds against, unpacked next to it.
# flatbuffers stays the SDK's own 1.11: the generated cvimodel header must
# match the flatbuffers headers the runtime is compiled with.
SOPHGO_CVIRUNTIME_DEPS = \
	flatbuffers:6da1cf79d90eb242e7da5318241d42279a3df3ba \
	cvibuilder:4309f2a649fc7cfe7160389d52a81c469dbdd7bc \
	cvikernel:9f1f57a19c3c281a931dfc71b318494487193d56
SOPHGO_CVIRUNTIME_EXTRA_DOWNLOADS = $(foreach d,$(SOPHGO_CVIRUNTIME_DEPS),\
	https://github.com/sophgo/$(word 1,$(subst :, ,$(d)))/archive/$(word 2,$(subst :, ,$(d))).tar.gz)
define SOPHGO_CVIRUNTIME_ADD_DEPS
	$(foreach d,$(SOPHGO_CVIRUNTIME_DEPS),\
		mkdir -p $(@D)/deps/$(word 1,$(subst :, ,$(d))) && \
		tar -xzf $(SOPHGO_CVIRUNTIME_DL_DIR)/$(word 2,$(subst :, ,$(d))).tar.gz --strip-components=1 \
			-C $(@D)/deps/$(word 1,$(subst :, ,$(d)))$(sep))
endef
SOPHGO_CVIRUNTIME_POST_EXTRACT_HOOKS += SOPHGO_CVIRUNTIME_ADD_DEPS

# old cmake_minimum_required() in the vendor trees
SOPHGO_CVIRUNTIME_CMAKE = $(BR2_CMAKE) -DCMAKE_POLICY_VERSION_MINIMUM=3.5
# build-machine compilers for flatc/cvibuilder: CMake wants a bare compiler
# path, so with BR2_CCACHE the "ccache gcc" of HOSTCC goes in as a launcher
SOPHGO_CVIRUNTIME_HOST_CC = -DCMAKE_C_COMPILER="$(HOSTCC_NOCCACHE)" \
	-DCMAKE_CXX_COMPILER="$(HOSTCXX_NOCCACHE)"
ifeq ($(BR2_CCACHE),y)
SOPHGO_CVIRUNTIME_HOST_CC += -DCMAKE_C_COMPILER_LAUNCHER=$(HOST_DIR)/bin/ccache \
	-DCMAKE_CXX_COMPILER_LAUNCHER=$(HOST_DIR)/bin/ccache
endif
SOPHGO_CVIRUNTIME_TARGET_CMAKE = $(SOPHGO_CVIRUNTIME_CMAKE) \
	-DCMAKE_TOOLCHAIN_FILE=$(HOST_DIR)/share/buildroot/toolchainfile.cmake \
	-DCMAKE_BUILD_TYPE=Release \
	-DCMAKE_INSTALL_PREFIX=$(@D)/sdk
SOPHGO_CVIRUNTIME_JOBS = -j$(PARALLEL_JOBS)

define SOPHGO_CVIRUNTIME_BUILD_CMDS
	# 1. flatc for the build machine
	$(SOPHGO_CVIRUNTIME_CMAKE) -S $(@D)/deps/flatbuffers -B $(@D)/b-flatc \
		$(SOPHGO_CVIRUNTIME_HOST_CC) \
		-DCMAKE_BUILD_TYPE=Release -DCMAKE_CXX_FLAGS="-w" \
		-DFLATBUFFERS_BUILD_TESTS=OFF -DFLATBUFFERS_BUILD_FLATLIB=OFF \
		-DFLATBUFFERS_BUILD_FLATHASH=OFF -DCMAKE_INSTALL_PREFIX=$(@D)/host-fb
	$(MAKE) $(SOPHGO_CVIRUNTIME_JOBS) -C $(@D)/b-flatc install
	# 2. cvimodel.fbs / parameter.fbs -> C++ headers
	$(SOPHGO_CVIRUNTIME_CMAKE) -S $(@D)/deps/cvibuilder -B $(@D)/b-cvibuilder \
		$(SOPHGO_CVIRUNTIME_HOST_CC) \
		-DFLATBUFFERS_PATH=$(@D)/host-fb -DCMAKE_INSTALL_PREFIX=$(@D)/cvibuilder-inst
	$(MAKE) -C $(@D)/b-cvibuilder install
	# 3. target libraries
	$(SOPHGO_CVIRUNTIME_TARGET_CMAKE) -S $(@D)/deps/cvikernel -B $(@D)/b-cvikernel -DCHIP=cv181x
	$(MAKE) $(SOPHGO_CVIRUNTIME_JOBS) -C $(@D)/b-cvikernel install
	# -Wno-error=unused-variable: variables read only by assert(), gone in
	# the release (NDEBUG) build
	$(SOPHGO_CVIRUNTIME_TARGET_CMAKE) -S $(@D) -B $(@D)/b-runtime \
		-DCHIP=cv181x -DRUNTIME=SOC -DENABLE_TEST=OFF -DENABLE_TOOLS=OFF \
		-DCMAKE_CXX_FLAGS="-Wno-error=unused-variable" \
		-DCVIKERNEL_PATH=$(@D)/sdk \
		-DFLATBUFFERS_PATH=$(@D)/deps/flatbuffers \
		-DCVIBUILDER_PATH=$(@D)/cvibuilder-inst
	$(MAKE) $(SOPHGO_CVIRUNTIME_JOBS) -C $(@D)/b-runtime install
endef

define SOPHGO_CVIRUNTIME_INSTALL_STAGING_CMDS
	$(INSTALL) -d $(STAGING_DIR)/usr/include/sophgo-tpu
	cp -a $(@D)/sdk/include/. $(STAGING_DIR)/usr/include/sophgo-tpu/
	cp -a $(@D)/sdk/lib/libcviruntime.so $(@D)/sdk/lib/libcvikernel.so $(STAGING_DIR)/usr/lib/
endef

define SOPHGO_CVIRUNTIME_INSTALL_TARGET_CMDS
	cp -a $(@D)/sdk/lib/libcviruntime.so $(@D)/sdk/lib/libcvikernel.so $(TARGET_DIR)/usr/lib/
endef

$(eval $(generic-package))

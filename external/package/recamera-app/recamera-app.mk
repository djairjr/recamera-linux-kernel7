################################################################################
#
# recamera-app -- the Rust application (app/ workspace of this tree)
#
################################################################################

RECAMERA_APP_VERSION = 0.1.0
RECAMERA_APP_SITE = $(BR2_EXTERNAL_RECAMERA_PATH)/../app
RECAMERA_APP_SITE_METHOD = local
RECAMERA_APP_LICENSE = MIT or Apache-2.0
RECAMERA_APP_LICENSE_FILES = LICENSE-MIT LICENSE-APACHE

# app/sophgo-sys links librecamera-cam.a, the vendor MPI libraries and the
# NPU runtime from the sysroot (sophgo-mpi, sophgo-cviruntime).
RECAMERA_APP_DEPENDENCIES = sophgo-mpi sophgo-cviruntime openssl
# WebRTC (str0m, "openssl" backend): openssl-sys finds the OpenSSL 3 of the
# sysroot here instead of probing the host
RECAMERA_APP_CARGO_ENV = RECAMERA_SYSROOT=$(STAGING_DIR) \
	OPENSSL_DIR=$(STAGING_DIR)/usr \
	OPENSSL_LIB_DIR=$(STAGING_DIR)/usr/lib \
	OPENSSL_INCLUDE_DIR=$(STAGING_DIR)/usr/include

# The binaries the workspace builds and the image installs.
RECAMERA_APP_BINS = recamerad recamera-events recamera-yolo

# Buildroot vendors the crates of a cargo package at download time, which a
# local package does not have: vendor right after the rsync, into the build
# dir, with the shared cargo cache of dl/ (network only when Cargo.lock
# changes); the build then runs --offline --locked as for any cargo package.
define RECAMERA_APP_VENDOR
	mkdir -p $(@D)/.cargo $(BR_CARGO_HOME)
	cd $(@D) && CARGO_HOME=$(BR_CARGO_HOME) $(HOST_DIR)/bin/cargo vendor \
		--locked VENDOR > .cargo/config.toml
endef
RECAMERA_APP_POST_RSYNC_HOOKS += RECAMERA_APP_VENDOR

# cargo install cannot take a virtual workspace: install the release binaries
# the build made. fence.tso count per binary: the C906 traps on it, the board
# OpenSBI emulates it (0.32 us); LLVM only emits it for fence(AcqRel)
# (docs/application.md §1).
define RECAMERA_APP_INSTALL_TARGET_CMDS
	$(foreach b,$(RECAMERA_APP_BINS),\
		$(INSTALL) -D -m 0755 $(@D)/target/$(RUSTC_TARGET_NAME)/release/$(b) \
			$(TARGET_DIR)/usr/bin/$(b) && \
		printf 'recamera-app: %s %s bytes, fence.tso: %s\n' $(b) \
			$$(stat -c %s $(TARGET_DIR)/usr/bin/$(b)) \
			$$($(TARGET_OBJDUMP) -d $(TARGET_DIR)/usr/bin/$(b) | grep -c 'fence\.tso' || true);)
endef

$(eval $(cargo-package))

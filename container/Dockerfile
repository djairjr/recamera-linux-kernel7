# SPDX-License-Identifier: GPL-2.0
#
# Build image for the reCamera (SG2002) project.
#
# The image carries a riscv64/musl cross toolchain that Buildroot itself built
# (the revision the project fixes: 2026.05.3), so the project build never
# depends on a toolchain installed on the host.  This is the route the manual
# documents: "Using the generated toolchain outside Buildroot" (the tarball
# produced by `make sdk` "can now be re-used as an external toolchain in other
# Buildroot projects").
#
# The toolchain is built for the same target the project declares in
# external/configs/sg2002_recamera_emmc_defconfig: riscv64, custom ISA IMAFDC,
# 64-bit, LP64D -- see container/sdk-defconfig, which must be kept in step with
# that architecture block.
#
# Inside the image:
#   /opt/riscv64-musl   the SDK (equal to the host/ directory of the toolchain build)
#   /work               where the project tree is mounted
#
# The tree is mounted from the host and is built as UID/GID 1000 by default
# (container/run.sh passes the caller's uid:gid): nothing is ever built as root,
# so no root-owned file is left inside the tree -- the failure that cost the
# previous route a privileged pass over 971079 files.  The toolchain build obeys
# the same rule: the manual requires a normal user ("Buildroot should not be run
# as root"), and host-tar refuses to configure as root at all.

ARG DEBIAN_RELEASE=bookworm
ARG BUILDROOT_TAG=2026.05.3
ARG BUILDROOT_REV=93e8f75c559a6efd6d4b503123e4755bb2a49576
ARG BUILDER_UID=1000
ARG BUILDER_GID=1000

# ---------------------------------------------------------------------------
# Host prerequisites: the mandatory packages of the manual's chapter 2
# ("System requirements"), plus libncurses-dev for menuconfig and git to fix
# the Buildroot revision.
# ---------------------------------------------------------------------------
FROM debian:${DEBIAN_RELEASE}-slim AS prerequisites
ARG BUILDER_UID
ARG BUILDER_GID
ENV DEBIAN_FRONTEND=noninteractive
RUN apt-get update \
 && apt-get install -y --no-install-recommends \
      bc \
      binutils \
      bison \
      build-essential \
      bzip2 \
      ca-certificates \
      cpio \
      diffutils \
      file \
      findutils \
      flex \
      g++ \
      gawk \
      gcc \
      git \
      gzip \
      libncurses-dev \
      make \
      patch \
      perl \
      python3 \
      rsync \
      sed \
      tar \
      unzip \
      wget \
      which \
 && rm -rf /var/lib/apt/lists/* \
 && groupadd --gid "${BUILDER_GID}" builder \
 && useradd --uid "${BUILDER_UID}" --gid "${BUILDER_GID}" \
      --create-home --shell /bin/bash builder \
 && mkdir -p /build /work \
 && chown builder:builder /build /work

# ---------------------------------------------------------------------------
# The toolchain: a minimal Buildroot build whose only product is the SDK.
# ---------------------------------------------------------------------------
FROM prerequisites AS toolchain-builder
ARG BUILDROOT_TAG
ARG BUILDROOT_REV
USER builder
WORKDIR /build
COPY --chown=builder:builder sdk-defconfig /build/sdk-defconfig
# GCC patches (BR2_GLOBAL_PATCH_DIR of sdk-defconfig): fence.tso off by
# default, the C906 traps on it (docs/multimedia.md §6.2)
COPY --chown=builder:builder patches /build/patches
RUN git clone --depth 1 --branch "${BUILDROOT_TAG}" \
        https://gitlab.com/buildroot.org/buildroot.git /build/buildroot \
 && test "$(git -C /build/buildroot rev-parse HEAD)" = "${BUILDROOT_REV}" \
 && mkdir -p /build/output \
 && make -C /build/buildroot O=/build/output \
        BR2_DEFCONFIG=/build/sdk-defconfig defconfig \
 && make -C /build/buildroot O=/build/output sdk

# ---------------------------------------------------------------------------
# The image we build in: prerequisites + the SDK, at a fixed path, relocated.
# ---------------------------------------------------------------------------
FROM prerequisites AS build-image
ARG BUILDER_UID
ARG BUILDER_GID
COPY --from=toolchain-builder /build/output/images/*_sdk-buildroot.tar.gz /tmp/sdk.tar.gz
RUN mkdir -p /opt \
 && tar xzf /tmp/sdk.tar.gz -C /opt \
 && mv /opt/*_sdk-buildroot /opt/riscv64-musl \
 && /opt/riscv64-musl/relocate-sdk.sh \
 && rm -f /tmp/sdk.tar.gz \
 && printf '%s\n' \
      'PATH=/opt/riscv64-musl/bin:$PATH' \
      > /etc/profile.d/toolchain.sh

ENV PATH=/opt/riscv64-musl/bin:$PATH
WORKDIR /work

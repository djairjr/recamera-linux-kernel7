#!/bin/sh
# SPDX-License-Identifier: GPL-2.0
#
# Buildroot post-build script (manual, 9.5): runs on the target tree before
# the rootfs images are made.
#
# /etc/recamera-version: which build this rootfs is (also the VERSION of the
# card's INSTALL set, copied by post-image.sh), so each eMMC slot says what it
# runs: cat /etc/recamera-version. Base of the programs' --version.
set -eu

TARGET_DIR=${1:?usage: post-build.sh <target dir>}
echo "reCamera 7.2 $(date -u +%Y-%m-%dT%H:%M:%SZ)" > "${TARGET_DIR}/etc/recamera-version"

# Site files kept out of git (projeto/local/): the Wi-Fi network of this
# installation replaces the public, network-less wpa_supplicant.conf.
LOCAL=$(cd "$(dirname "$0")/../../../.." && pwd)/local
if [ -f "$LOCAL/wpa_supplicant.conf" ]; then
	install -m 0600 "$LOCAL/wpa_supplicant.conf" "${TARGET_DIR}/etc/wpa_supplicant.conf"
	echo "post-build: Wi-Fi network from local/wpa_supplicant.conf"
fi

# No mailboxes on a camera: login (shadow) would print "No mail." at every
# login, the page's terminal included.
if [ -f "${TARGET_DIR}/etc/login.defs" ]; then
	sed -i 's/^MAIL_CHECK_ENAB\b.*/MAIL_CHECK_ENAB\t\tno/' "${TARGET_DIR}/etc/login.defs"
fi

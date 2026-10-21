#!/bin/sh
# SPDX-License-Identifier: GPL-2.0
#
# cam.sh -- run ssh/scp against the camera without typing the password.
#
#   scripts/cam.sh ssh 'uname -a'          command on the camera
#   scripts/cam.sh ssh < script.sh         ... or a script on stdin
#   scripts/cam.sh scp <src> :<dst>        copy to the camera (":" = camera)
#   scripts/cam.sh scp :<src> <dst>        copy from the camera
#
# Environment: RECAMERA_HOST (default 192.168.42.1, USB-C network),
#   RECAMERA_USER (default root), RECAMERA_PASS (default: the image's
#   documented default password). The host key is not stored: the camera
#   gets a new one at every reinstall.
set -eu

HOST=${RECAMERA_HOST:-192.168.42.1}
USER_=${RECAMERA_USER:-root}

# ssh asks for the password through SSH_ASKPASS: this same script answers.
if [ "${CAM_SH_ASKPASS:-}" = 1 ]; then
	echo "${RECAMERA_PASS:-recamera}"
	exit 0
fi
export CAM_SH_ASKPASS=1 SSH_ASKPASS="$(cd "$(dirname "$0")" && pwd)/$(basename "$0")"
export SSH_ASKPASS_REQUIRE=force DISPLAY=${DISPLAY:-:0}
OPTS="-o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null -o LogLevel=ERROR -o ConnectTimeout=5"

cmd=${1:?usage: cam.sh ssh|scp ...}
shift
case "$cmd" in
	ssh)
		exec ssh $OPTS "$USER_@$HOST" "$@" ;;
	scp)
		# ":path" means the camera; -O: the image has no sftp-server
		args=""
		for a in "$@"; do
			case "$a" in :*) a="$USER_@$HOST$a" ;; esac
			args="$args '$a'"
		done
		eval exec scp -O $OPTS $args ;;
	*)
		echo "cam.sh: unknown command $cmd (ssh|scp)" >&2; exit 2 ;;
esac

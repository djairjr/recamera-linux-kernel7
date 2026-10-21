#!/bin/sh
# SPDX-License-Identifier: GPL-2.0
#
# models.sh -- the YOLO .cvimodel files of the camera, kept OUT of the image
# and of the repository (Ultralytics weights are AGPL-3.0): they live in the
# persistent /data/models of the camera (docs/application.md).
#
#   scripts/models.sh extract            the 4 factory models (YOLO11n int8,
#                                        cv181x: detection, classify, pose,
#                                        segment) out of the official rootfs
#                                        into work/models/factory/, md5 checked
#                                        against the checksum of their .json
#   scripts/models.sh install [dir]      copy <dir>/*.cvimodel (+ .json) to the
#                                        camera's /data/models, md5 checked there
#                                        (default dir: work/models/factory)
#
# Source of "extract": sd_compat_rootfs.ext4, the rootfs partition of the
# official sg2002_reCamera_0.2.4_emmc_sd_compat.img (Seeed-Studio/reCamera-OS
# release 0.2.4), in the directory RECAMERA_OFFICIAL (default ../oficial next
# to this tree), mounted at /official by container/run.sh. Read with the
# Buildroot host debugfs.
set -eu

HERE=$(cd "$(dirname "$0")" && pwd)
P=$(cd "$HERE/.." && pwd)
CAM="$HERE/cam.sh"
OUT=$P/work/models/factory
SRC_IMG=/official/sd_compat_rootfs.ext4
SRC_DIR=/usr/share/supervisor/models
NAMES="yolo11n_detection_cv181x_int8 yolo11n_classify_cv181x_int8 yolo11n_pose_cv181x_int8 yolo11n_segment_cv181x_int8"

case "${1:-}" in
	extract)
		mkdir -p "$OUT"
		"$P/container/run.sh" sh -c "set -e; D=/work/output/sg2002_recamera_emmc/host/sbin/debugfs
			for n in $NAMES; do for e in cvimodel json; do
				\$D -R \"dump $SRC_DIR/\$n.\$e /work/work/models/factory/\$n.\$e\" $SRC_IMG 2>/dev/null
			done; done"
		for n in $NAMES; do
			want=$(python3 -I -c "import json,sys; print(json.load(open(sys.argv[1]))['checksum'])" "$OUT/$n.json")
			got=$(md5sum "$OUT/$n.cvimodel" | cut -d' ' -f1)
			[ "$want" = "$got" ] || { echo "md5 mismatch: $n ($got, json says $want)" >&2; exit 1; }
			echo "ok $n.cvimodel $got"
		done
		;;
	install)
		D=${2:-$OUT}
		ls "$D"/*.cvimodel > /dev/null
		"$CAM" ssh "mkdir -p /data/models"
		for f in "$D"/*.cvimodel "$D"/*.json; do
			[ -e "$f" ] || continue
			"$CAM" scp "$f" ":/data/models/"
		done
		sums=$(cd "$D" && md5sum *.cvimodel)
		"$CAM" ssh "cd /data/models && echo '$sums' | md5sum -c - && sync"
		;;
	*)
		echo "usage: models.sh extract | install [dir]" >&2; exit 2 ;;
esac

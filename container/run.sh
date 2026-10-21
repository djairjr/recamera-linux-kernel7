#!/bin/sh
# SPDX-License-Identifier: GPL-2.0
#
# Run a command inside the project's build container, with the project tree
# mounted at /work.  The command runs as the caller's uid:gid, so every file
# the build writes inside the tree stays owned by the caller on the host.
#
# Examples:
#   container/run.sh make sg2002_recamera_emmc_defconfig
#   container/run.sh make list-defconfigs
#   container/run.sh sh

set -eu

image=${RECAMERA_BUILD_IMAGE:-recamera-build:latest}
project_dir=$(cd "$(dirname "$0")/.." && pwd)

if [ "$(docker image inspect -f '{{.Id}}' "$image" 2>/dev/null || true)" = "" ]; then
	printf 'container/run.sh: image %s not found; build it with:\n' "$image" >&2
	printf '  docker build -t %s %s/container\n' "$image" "$project_dir" >&2
	exit 1
fi

# -i keeps stdin open; -t is only accepted when stdin really is a terminal
# (docker refuses it otherwise), so a non-interactive caller gets -i alone.
if [ -t 0 ]; then
	tty_flags=-it
else
	tty_flags=-i
fi

# The official Seeed release files (images, factory rootfs), read-only at
# /official when present: the source of the factory models (scripts/models.sh).
official_dir=${RECAMERA_OFFICIAL:-$project_dir/../oficial}
official_mount=""
if [ -d "$official_dir" ]; then
	official_mount="-v $(cd "$official_dir" && pwd):/official:ro"
fi

exec docker run --rm $tty_flags \
	--user "$(id -u):$(id -g)" \
	-e HOME=/tmp \
	-v "$project_dir":/work \
	$official_mount \
	-w /work \
	"$image" "$@"

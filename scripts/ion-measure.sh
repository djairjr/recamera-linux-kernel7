#!/bin/sh
# SPDX-License-Identifier: GPL-2.0
#
# ion-measure.sh -- peak ION (carveout heap) use of a command run on the
# camera: samples the heap every 200 ms while it runs, prints the highest use,
# the allocation list at that moment, the command's last lines, its exit
# status, and the heap after it (must be back to all free). F7 step 10.
#
#   scripts/ion-measure.sh '<command on the camera>'
#   e.g. scripts/ion-measure.sh 'VICAP_ONLINE=1 recamera-vicap 30 /data/y yolo 60'
set -eu

HERE=$(cd "$(dirname "$0")" && pwd)
CMD=${1:?usage: ion-measure.sh '<command>'}

"$HERE/cam.sh" ssh sh -s <<EOF
D=/sys/kernel/debug/ion/cvi_carveout_heap_dump
grep -q debugfs /proc/mounts || mount -t debugfs none /sys/kernel/debug
total=\$(cat \$D/total_mem)
( $CMD ) > /tmp/ion-measure.log 2>&1 &
pid=\$!
max=0
while kill -0 \$pid 2>/dev/null; do
	u=\$(cat \$D/alloc_mem)
	if [ "\$u" -gt "\$max" ]; then
		max=\$u
		sed -n '/Details/,/minimum/p' \$D/summary | grep -vE 'Details|minimum|heap_id|^\s*\$' > /tmp/ion-measure.peak
	fi
	sleep 0.2
done
wait \$pid; rc=\$?
echo "== command: $CMD"
grep -E '^(enc|yolo|all|frame 0|VI ->|VPSS|online)|fail|0x[0-9a-f]{8}' /tmp/ion-measure.log | grep -v '^frame [1-9]' | tail -6
echo "== exit \$rc"
echo "== ION peak \$((max / 1024)) KiB of \$((total / 1024)) KiB"
awk '{ printf "   %-22s %8d KiB\n", \$NF, \$2 / 1024 }' /tmp/ion-measure.peak
echo "== ION after: \$((\$(cat \$D/alloc_mem) / 1024)) KiB used"
EOF

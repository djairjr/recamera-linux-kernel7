#!/usr/bin/env bash
# watch-camera-sd.sh -- edge-triggered watcher for the reCamera coming back after
# the official SD recovery boot.
#
# It logs ONLY state changes, so the file stays readable:
#   - what appears on the USB bus (the internal laptop devices are filtered out)
#   - network interfaces that show up beside the usual ones
#   - whether 192.168.42.1 answers (that is the address the camera hands out to
#     the host over its own USB link, per the Seeed wiki)
#   - the kernel's own USB / mmc lines, verbatim, from journalctl -f
#
# Usage: watch-camera-sd.sh [seconds] [output file]
set -u

SECS=${1:-300}
OUT=${2:-$(cd "$(dirname "$0")/../.." && pwd)/work/serial/camera-$(date +%Y%m%d-%H%M%S).log}
mkdir -p "$(dirname "$OUT")"
INTERNAL='1d6b:0002|1d6b:0003|5986:215f|048d:c986|8087:0026'

exec >"$OUT" 2>&1

# Kernel side: the authoritative record of enumeration, in real time.
timeout "$SECS" journalctl -k -f -o short-precise 2>/dev/null \
    | grep --line-buffered -iE "usb [0-9]+-[0-9]+:|idVendor|rndis|cdc_acm|cdc_ncm|18d1|3346|mmcblk|sdhci|new .* interface" \
    | sed 's/^/[KERN] /' &
KPID=$!
trap 'kill $KPID 2>/dev/null' EXIT

start=$(date +%s)
echo "[INFO] vigia iniciado $(date +%T) por ${SECS}s"
echo "[INFO] filtro de devices internos: $INTERNAL"

prev_usb=""; prev_if=""; prev_ping=""
while :; do
    now=$(date +%s); el=$(( now - start )); [ "$el" -ge "$SECS" ] && break

    usb=$(lsusb | grep -Ev "$INTERNAL" | sed 's/^Bus /bus /' | sort | tr '\n' '|')
    if [ "$usb" != "$prev_usb" ]; then
        echo "[$(date +%T.%3N)] t=${el}s  USB: ${usb:-<nada alem dos internos>}"
        prev_usb="$usb"
    fi

    ifc=$(ip -brief link | awk '{print $1"="$2}' | grep -Ev '^(lo|wl|enp|docker|br-|veth)' | sort | tr '\n' ' ')
    if [ "$ifc" != "$prev_if" ]; then
        echo "[$(date +%T.%3N)] t=${el}s  IFACES: ${ifc:-<nenhuma>}"
        prev_if="$ifc"
    fi

    if ping -c1 -W1 192.168.42.1 >/dev/null 2>&1; then p=sim; else p=nao; fi
    if [ "$p" != "$prev_ping" ]; then
        echo "[$(date +%T.%3N)] t=${el}s  192.168.42.1 responde: $p"
        prev_ping="$p"
    fi

    sleep 0.3
done
echo "[INFO] vigia encerrado $(date +%T)"

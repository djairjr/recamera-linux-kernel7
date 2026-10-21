#!/usr/bin/env bash
# watch-boot.sh -- edge-triggered watcher for one boot attempt of the camera.
#
# Reports only transitions, because the answer is which of three states the
# board ends in, not a stream of samples:
#
#   3346:100c  "NCM"          -> the board booted (kernel up, USB net gadget)
#   18d1:4ee0                 -> U-Boot reached its USB gadget stage
#   3346:1000  "USB Com Port" -> the board fell back to ROM download mode,
#                                which means it did NOT find a bootable fip
#
# Usage: watch-boot.sh <logfile> [seconds]
set -u
LOG=${1:?uso: watch-boot.sh <logfile> [seconds]}
DUR=${2:-900}
HOSTIF_RE='^(lo|enp8s0|wlp0s20f3|docker0|br-|veth)'

{
  echo "== watch start $(date +%T) dur=${DUR}s =="
  prev_usb=""
  prev_ip=""
  saw_pong=""
  end=$(( SECONDS + DUR ))
  while [ $SECONDS -lt $end ]; do

    # 1. what the board presents on USB (only the two IDs that mean something)
    cur_usb=$(lsusb 2>/dev/null | grep -oE '(3346:1000|3346:100c|18d1:4ee0)' | sort -u | tr '\n' ',' )
    if [ "$cur_usb" != "$prev_usb" ]; then
      echo "[$(date +%T)] USB: ${cur_usb:-<nenhum 3346/18d1>}"
      prev_usb="$cur_usb"
    fi

    # 2. did the host interface get an address (the camera is the DHCP server)
    cur_ip=$(ip -brief addr show 2>/dev/null | grep -vE "$HOSTIF_RE" | awk '{print $1" "$3}' | tr '\n' ' ')
    if [ "$cur_ip" != "$prev_ip" ]; then
      echo "[$(date +%T)] IFACE: ${cur_ip:-<nenhuma>}"
      prev_ip="$cur_ip"
    fi

    # 3. can we reach the camera
    if ping -c1 -W1 192.168.42.1 >/dev/null 2>&1; then
      if [ -z "$saw_pong" ]; then
        echo "[$(date +%T)] *** CAMERA RESPONDE EM 192.168.42.1 ***"
        for p in 22 80 554 1883 9090; do
          (exec 3<>/dev/tcp/192.168.42.1/$p) >/dev/null 2>&1 \
            && echo "     porta $p aberta" || echo "     porta $p fechada"
        done
        saw_pong=1
      fi
    else
      saw_pong=""
    fi

    sleep 2
  done
  echo "== watch end $(date +%T) (ultimo USB: ${prev_usb:-<nenhum>}) =="
} >>"$LOG" 2>&1

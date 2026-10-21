# recamera-motd -- the System page of the web interface, at an interactive
# login (ssh, console): version, slot, addresses, uptime, temperature, load,
# memory and the daemon. Only for a terminal: scp and remote commands get
# nothing.
[ -t 1 ] && case "$-" in *i*) true ;; *) false ;; esac && {
	_up=$(cut -d. -f1 /proc/uptime)
	_d=$((_up / 86400)); _h=$((_up % 86400 / 3600)); _m=$((_up % 3600 / 60))
	_temp=$(awk '{ printf "%.1f", $1 / 1000 }' /sys/class/thermal/thermal_zone0/temp 2>/dev/null)
	_load=$(cut -d' ' -f1-3 /proc/loadavg)
	_mem=$(awk '/MemTotal/ { t = $2 } /MemAvailable/ { a = $2 } END { printf "%d MB free of %d MB", a / 1024, t / 1024 }' /proc/meminfo)
	_slot=$(sed -n 's/.*recamera\.slot=\([ab]\).*/\1/p' /proc/cmdline | tr ab AB)
	_ver=$(sed 's/^reCamera //' /etc/recamera-version 2>/dev/null)
	_ips=$(ip -4 -o addr show 2>/dev/null | awk '$2 != "lo" { sub("/.*", "", $4); printf "%s%s (%s)", s, $4, $2; s = ", " }')
	_ip1=$(ip -4 -o addr show wlan0 2>/dev/null | awk '{ sub("/.*", "", $4); print $4; exit }')
	[ -n "$_ip1" ] || _ip1=$(ip -4 -o addr show 2>/dev/null | awk '$2 != "lo" { sub("/.*", "", $4); print $4; exit }')
	_pid=$(pidof recamerad)
	if [ -n "$_pid" ]; then
		_rss=$(awk '/VmRSS/ { printf "%.1f MB", $2 / 1024 }' /proc/$_pid/status 2>/dev/null)
		_daemon="running (pid $_pid, $_rss)"
	else
		_daemon="not running"
	fi
	_dt=""; [ $_d -gt 0 ] && _dt="$_d d "
	printf '\n'
	printf '   \033[1;34m.---.\033[0m     \033[1mreCamera\033[0m    %s\n' "$_ver"
	printf '  \033[1;34m( \033[1;36m(o)\033[1;34m )\033[0m    Boot slot   %s\n' "${_slot:-SD}"
	printf '   \033[1;34m`---`\033[0m     Addresses   %s\n' "${_ips:-none}"
	printf '             Uptime      %s%s h %s min\n' "$_dt" "$_h" "$_m"
	printf '             SoC         %s °C, load %s\n' "${_temp:-?}" "$_load"
	printf '             Memory      %s\n' "$_mem"
	printf '             recamerad   %s\n' "$_daemon"
	[ -n "$_ip1" ] && printf '             Page        http://%s   RTSP rtsp://%s/live\n' "$_ip1" "$_ip1"
	printf '\n'
	unset _up _d _h _m _temp _load _mem _slot _ver _ips _ip1 _pid _rss _daemon _dt
}

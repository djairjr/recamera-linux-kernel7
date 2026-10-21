#!/bin/sh
# camdiag.sh -- snapshot of camera-related HW state (same script on the
# factory 5.10 system and on our 7.2 system, for diffing).
#
# Usage: scripts/cam.sh ssh < scripts/camdiag.sh
echo "== kernel $(uname -r)  $(date)"
echo "== cmdline $(cat /proc/cmdline)"
echo "== modules"; lsmod 2>/dev/null | cut -c1-30
echo "== procs"; ps 2>/dev/null | grep -i -E 'sscma|supervisor|sample|vicap' | grep -v grep
r() { printf '%s=%s\n' "$1" "$(devmem $1 32 2>/dev/null)"; }
echo "== pinmux CAM_MCLK0/1, PWR_WAKEUP0(CAM_EN), PWR_GPIO1/2 (I2C2), MIPIRX5..0 N/P"
for a in 0x03001000 0x0300100C 0x03001090 0x03001164 0x03001168 0x0300116C 0x03001170 0x03001174 0x03001178 0x0300117C 0x03001180 0x03001184 0x03001188 0x0300118C 0x03001190; do r $a; done
echo "== pad ctrl MIPI (0x03001c30..0x03001c5c)"
for a in 0x03001C30 0x03001C34 0x03001C38 0x03001C3C 0x03001C40 0x03001C44 0x03001C48 0x03001C4C 0x03001C50 0x03001C54 0x03001C58 0x03001C5C; do r $a; done
echo "== clk regs: CLK_EN_0..4, CAM0 div 0x8C0, CAM1 0x8C4, PLL_G2 0x800, CAM0PLL_CSR 0x814, SSC 0x870/874"
for a in 0x03002000 0x03002004 0x03002008 0x0300200C 0x03002010 0x030028C0 0x030028C4 0x03002800 0x03002814 0x03002870 0x03002874 0x030020F4; do r $a; done
echo "== resets 0x03003000..0x0300300C"
for a in 0x03003000 0x03003004 0x03003008 0x0300300C; do r $a; done
echo "== vip_sys 0x0a0c8000..0x0a0c801c"
for a in 0x0A0C8000 0x0A0C8004 0x0A0C8008 0x0A0C800C 0x0A0C8010 0x0A0C8014 0x0A0C8018 0x0A0C801C; do r $a; done
echo "== csi wrap/dphy 0x0a0d0000.. (first 0x40)"
a=0; while [ $a -lt 64 ]; do r $(printf '0x%08X' $((0x0A0D0000 + a))); a=$((a+4)); done
echo "== csi mac0 0x0a0c2000.. (first 0x80)"
a=0; while [ $a -lt 128 ]; do r $(printf '0x%08X' $((0x0A0C2000 + a))); a=$((a+4)); done
echo "== /proc/mipi-rx"; cat /proc/mipi-rx 2>/dev/null
echo "== vi_dbg"; cat /proc/cvitek/vi_dbg 2>/dev/null | head -60
echo "== interrupts"; grep -i -E 'cif|vi|isp|mipi|csi' /proc/interrupts
echo "== sensor regs (i2c-2 0x36)"
for rr in 0x01:0x00 0x01:0x03 0x30:0x0A 0x30:0x0B 0x30:0x18 0x30:0x34 0x30:0x35 0x30:0x36 0x30:0x37 0x30:0x0D 0x48:0x00 0x48:0x37 0x42:0x02 0x38:0x08 0x38:0x09 0x38:0x0A 0x38:0x0B 0x30:0x00 0x30:0x01 0x30:0x02 0x30:0x16 0x30:0x17 0x30:0x1C 0x30:0x1D 0x50:0x00 0x40:0x05; do
  h=${rr%:*}; l=${rr#*:}; printf '%s%s=' $h ${l#0x}; i2ctransfer -f -y 2 w2@0x36 $h $l r1 2>&1; done
echo "== clk_summary (cam/csi/vip)"
mount -t debugfs none /sys/kernel/debug 2>/dev/null
grep -E 'cam|csi|vip|mipi|isp|raw' /sys/kernel/debug/clk/clk_summary 2>/dev/null | head -50
echo "== thermal $(cat /sys/class/thermal/thermal_zone0/temp 2>/dev/null)"

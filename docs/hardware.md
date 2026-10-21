# reCamera (SG2002) on Linux 7.2: the board hardware, how it works, how it was ported

This document describes the **real hardware of the board** and how each part works in the
port to Linux 7.2. Part of it was not documented anywhere, and cannot be seen on the
factory system without reading the Seeed code: the Bluetooth of the Wi-Fi chip, the
speaker, the microphone's dependency on the speaker's I2S clock, the MIPI lane mapping of
the camera, among others.

For each item: **factory** (what the original Seeed system, Linux 5.10, does) → **7.2**
(what was done) → **validation** (what was measured) → **pitfalls**.

---

## 1. Hardware map

| Block | Part / address | How Linux sees it | State on 7.2 |
|---|---|---|---|
| Wi-Fi 2.4/5 GHz | Cypress/Infineon **CYW43012** over SDIO, host `sdhci1` `0x4320000` | `wlan0` (brcmfmac) | ✅ |
| **Bluetooth 5.0 / BLE** | the **same CYW43012**, HCI over **UART1** (`ttyS1`) | `hci0` (BlueZ) | ✅ |
| **Microphone** | the SoC's **internal** audio ADC (`0x0300A100`) → I2S0 (`0x04100000`) | ALSA card 0 `recameramic`, `hw:0,0` | ✅ (muted by default) |
| **Speaker** | the SoC's **internal** audio DAC (`0x0300A000`) ← I2S3 (`0x04130000`) | ALSA card 1 `recameraspeaker`, `hw:1,0` | ✅ |
| Network over USB-C | DWC2 controller `0x4340000` in *device* mode, NCM gadget | `usb0` = 192.168.42.1 | ✅ |
| Ethernet | the SoC's internal PHY (CV182XA), out through an optional **GPIO→RJ45 adapter cable** | `eth0` | ✅ at 10 Mbps with that adapter (§6) |
| Reboot / power off | RTC block `0x05025000` | `reboot`, `poweroff` | ✅ |
| Watchdog | DesignWare WDT | `/dev/watchdog` | ✅ (patch 0023) |
| White LED | `PWR_GPIO0` → `PWM_8` | `/sys/class/leds/white` (0–255) | ✅ (patch 0022) |
| Clock | **no battery-backed RTC** | time from the network | ✅ (§8) |
| eMMC / SD card | `0x4300000` / `0x4310000` | `mmcblk0` / `mmcblk1` | ✅ |
| HaLow (802.11ah) | **optional** Morse Micro MM610x module (`fgh100m`) in the **SD slot** | out-of-tree `morse.ko` | ✅ ported, ⏳ not validated on hardware (§6b) |
| **SoC temperature** | internal `tempsen` `0x030E0000` (+ DDR refresh by temperature) | `thermal_zone0` (`soc-thermal`) | ✅ (§7b) |
| Camera / media | OV5647 sensor (MIPI **lanes {2,0,3}**, §7c), ISP, ION, codecs, NPU | `sophgo-osdrv` modules | ✅ see `docs/multimedia.md` |

An important shared pin: **WIFI_REG_ON = BT_REG_ON** (`gpio510` = `porta 30`), held high
by a `gpio-hog` in the DT. Powering the radio powers both cores (Wi-Fi and BT).

**Pins the vendor U-Boot muxes:** the vendor U-Boot (inside the fip) sets the pin mux of
`CAM_EN`, the camera I2C2, the ADC I2C1, UART1, the LEDs and the SD power enable in its
`cvi_board_init.c`. Linux 7.2 relies on that for several of them today; see
`docs/emmc-layout.md` §9 before changing the bootloader.

---

## 2. Wi-Fi (CYW43012)

- **Factory:** `brcmfmac` (plain upstream) + firmware `brcmfmac43012-sdio.{bin,txt,clm_blob}`;
  the SDIO host is the Cvitek driver `sdhci-cv181x`, which **never goes above 46.875 MHz**
  on the bus (375 MHz ÷ 8).
- **7.2:** mainline `brcmfmac` + the same firmware; the SDIO host is `sdhci-of-dwcmshc`,
  which reaches **exactly 50 MHz**.
- **Defect found:** at 50 MHz the SDIO bus, which runs next to the radio, **interferes with
  2.4 GHz channels 5–7**. An AP on channel 6 vanished from the scans and every join failed
  with `ASSOC-REJECT 16` (`SET_SSID status 1`), while channels 1 and 36 associated. Proven
  at runtime by changing one bus at a time (`/sys/kernel/debug/mmcX/clock`): only the
  Wi-Fi SDIO at 50 MHz breaks the join. The reference was the factory SD card in the same
  position, which always joined.
- **Fix:** `&sdhci1 { max-frequency = <37500000>; }` in the board DTS.
- **Validation:** joins by itself at boot, DHCP, 120/120 pings in 2 minutes.
- **Slow handshake at boot (21/10/2026):** with `wpa_supplicant` reading its configuration
  from `/data/recamera/wpa_supplicant.conf`, 2 boots of 2 took minutes to reach
  `COMPLETED` (handshakes ending back in `SCANNING`); with `/etc/wpa_supplicant.conf` (same
  contents, same image) 2 of 2 took 25 s. Why is still unknown. **Adopted:**
  `wpa_supplicant` reads `/etc`; the copy in `/data` (which survives A/B updates) is
  restored to `/etc` in the interface's `pre-up` and refreshed by `recamerad` after every
  change made on the page. Since then: `COMPLETED` within 20 s of power-on.

---

## 3. Bluetooth (CYW43012) — not documented before

- **Factory:** the `S41bt-init` script runs `brcm_patchram_plus --no2bytes --patchram
  CYW43012C0_003.001.015.0196.0269.hcd /dev/ttyS1`, then `hciattach /dev/ttyS1 any 115200
  flow` and `hciconfig hci0 up`. Measured on the factory system: `hci0 UP RUNNING`, the BD
  address is the Wi-Fi MAC minus one. The tool comes from `Seeed-Studio/sscma-example-sg200x`
  (`solutions/brcm-patchram-plus`).
- **7.2:**
  - kernel: `CONFIG_BT`, `BT_HCIUART`, `BT_HCIUART_H4`;
  - DT: UART1 with RTS/CTS (`uart1_cfg`: IIC0_SCL→TX, IIC0_SDA→RX, JTAG_CPU_TMS→RTS,
    JTAG_CPU_TCK→CTS);
  - firmware: the same `.hcd` as the factory (package `recamera-firmware`);
  - userland: `brcm-patchram-plus`, `bluez5_utils` (client, tools, deprecated commands), `dbus`;
  - service: `rootfs_overlay/etc/init.d/S41bt-hci`, the factory sequence (log `/var/log/bt-hci.log`).
- **Validation:** `hci0 UP RUNNING`, chip `CYW43012C0`, manufacturer Cypress, BlueZ 5.79;
  `bluetoothctl scan le` found BLE advertisers within 20 s.
- **Coexistence with Wi-Fi:** measured on the factory system: Wi-Fi associates the same
  with BT up or not.
- **Not used:** the mainline `hci_bcm` (serdev) driver has no entry for the 43012.

---

## 4. Audio: microphone and speaker — not documented before

### 4.1 The hardware

The SoC has an **internal audio codec**: an ADC for the microphone and a DAC for the
speaker. The reCamera 2002 spec lists 1 microphone and 1 speaker. The wiring comes from the
factory DT:

| Function | I2S block | Codec | DMA (dmamux) | Factory format |
|---|---|---|---|---|
| Capture (mic) | **I2S0** `0x04100000`, RX only | ADC `0x0300A100` | request 0 (`CVI_I2S0_RX`), CPU 1 | `I2S, IB_NF, CBM_CFM` → **the ADC is the clock master** |
| Playback (speaker) | **I2S3** `0x04130000`, TX only | DAC `0x0300A000` | request 7 (`CVI_I2S3_TX`), CPU 1 | `I2S, IB_IF, CBS_CFS` → I2S3 is the master |

**Hidden dependency:** the ADC **has no clock of its own**. It uses the **MCLK output of
I2S3** (factory DT: `clk_source = <0x4130000>`, I2S3 `mclk_out = "true"`). The factory ADC
driver turns that MCLK on before capturing.

### 4.2 On 7.2

Mainline drivers (`sound/soc/sophgo/`): `cv1800b-tdm` (I2S), `cv1800b-sound-adc`,
`cv1800b-sound-dac`, plus `simple-audio-card`, `dw-axi-dmac` and `cv1800b-dmamux`. The
audio nodes **do not exist** in the mainline SoC `.dtsi`; they are in the board DTS.

Problems found and fixed, in order:

1. **DMA on the wrong channel** → `arecord: I/O error`. The `.dtsi` declares the DMA as a
   generic `snps,axi-dma-1.01a`, and the driver takes any free channel, but the `dmamux`
   ties each request to a fixed channel. Fix in the DTS:
   `&dmac { compatible = "sophgo,cv1800b-axi-dma"; }`, which turns on
   `AXI_DMA_FLAG_ARG0_AS_CHAN` in the driver.
2. **Capture full of random full-scale noise.** Two causes:
   - the clock master was inverted: I2S0 was the master and the ADC made its own
     BCLK/LRCLK. Fix in the DTS: on the mic card, `bitclock-master`/`frame-master` =
     **codec**, as on the factory system;
   - no MCLK for the ADC while the speaker is idle. Fix: **patch 0011**: the ADC driver
     gets `sophgo,mclk-source = <&i2s3>` and the `mclk` clock (`CLK_SDMA_AUD3`), and turns
     the I2S3 MCLK (256·fs) on before capturing, like the factory driver.
3. **Microphone muted by default** (a project requirement): **patch 0010**. The ADC volume
   goes from 0 to +48 dB and **0 is not silence**; the patch exposes the PGA mute bits as
   `Internal I2S Capture Switch` and leaves the microphone **off at probe**, i.e. at every
   boot.
4. Kernel: `CONFIG_DW_AXI_DMAC`, `SOPHGO_CV1800B_DMAMUX`, `SND_SOC_CV1800B_TDM`,
   `SND_SOC_CV1800B_ADC_CODEC`, `SND_SOC_CV1800B_DAC_CODEC`, `SND_SIMPLE_CARD`. Userland:
   `alsa-utils`.

### 4.3 Validation

- Cards: card 0 `recamera-mic` (`hw:0,0`), card 1 `recamera-speaker` (`hw:1,0`), the Seeed
  numbering.
- Mic muted after boot: switch `off,off`; a **muted** recording → peak 3, mean 0 (hardware
  silence).
- Mic on, **nothing playing**: room mean 40–115; hand claps peak at 32,700.
- Speaker: a 1 kHz tone, heard.
- **Full loop** (1 kHz on the speaker, recorded by the microphone): **2000 zero crossings
  per second = 1 kHz**, steady for 3 s.

### 4.4 Using it

```sh
# turn the microphone on (it starts muted at every boot) and set the gain (0..24 = 0..+48 dB)
amixer -c recameramic cset name='Internal I2S Capture Switch' on,on
amixer -c recameramic cset name='Internal I2S Capture Volume' 6,6
arecord -D hw:0,0 -f S16_LE -r 48000 -c 2 recording.wav
amixer -c recameramic cset name='Internal I2S Capture Switch' off,off   # muted again

# speaker (48 kHz, S16_LE, stereo only; the DAC has no volume control)
aplay -D hw:1,0 sound_48k_s16_stereo.wav
```

Known, harmless log noise (only at the end of each stream) was removed by patches 0012
(I2S accepts `set_sysclk(0)`) and 0013 (no "apb_regs not initialized" in fixed-channel mode).

---

## 5. Network over USB-C (192.168.42.1)

- **Factory:** `S70hardware` → `usb-net.sh` → `uhubon.sh device` (`/proc/cviusb/otg_role`)
  + `run_usb.sh probe/start ncm` (configfs gadget "cvitek", VID `0x3346`, PID `0x100C`,
  "Cvitek"/"NCM"); `usb0` 192.168.42.1; `dnsmasq usb_only.conf` (192.168.42.10–242).
- **7.2:** DT `&usb { dr_mode = "peripheral"; }`; `rootfs_overlay/etc/init.d/S45usbnet` (the
  same NCM gadget over configfs, the same IDs, fixed MACs derived from the eFuse one);
  BusyBox `udhcpd` with `etc/udhcpd-usb0.conf` (the same range).
- **Validation:** UDC `configured` (high speed); the laptop gets 192.168.42.10; ~2 ms ping.
- **Gadget DMA (patch 0015):** with the factory values (`g_dma = true`,
  `g_dma_desc = true`) laptop → camera works, but **TCP camera → laptop breaks**: the SSH
  banner never arrives. That is the known `dwc2` descriptor-DMA alignment limit, which the
  vendor works around inside `gadget.c`. **Buffer DMA** (`g_dma = true`,
  `g_dma_desc = false`) works: `iperf3` **157 Mbit/s** camera receiving and **146 Mbit/s**
  sending, against 122/94 without DMA.
- **Slow SSH login** was dropbear's reverse-DNS lookup of the client on a network without
  DNS (the USB link): fixed with `BR2_PACKAGE_DROPBEAR_DISABLE_REVERSEDNS=y`.

---

## 6. Ethernet (GPIO→RJ45 adapter cable)

- The PHY is the **SoC's internal one** (CV182XA); its driver is the vendor's, ported
  (patch 0001), faithful to the original.
- **Defect (on the factory system too):** with the adapter, the link at **100 Mbps comes up
  and drops within ~1 s, receiving nothing**. At 10 Mbps the same cable works cleanly:
  straight to a laptop, 0 % loss, zero errors. The network cable and the switch port were
  cleared with the laptop. **Cause:** the adapter does not sustain 100BASE-TX
  (magnetics, impedance or pairs).
- **A fix that does not cap the speed:** `rootfs_overlay/etc/init.d/S42eth-downshift` +
  `ethtool`. It always tries 100 Mbps; if the 100 Mbps link drops 3 times in 30 s without
  RX, it advertises 10 Mbps only; with the cable unplugged for 10 s it goes back to 100. A
  better adapter gets 100 Mbps by itself.
- **Validation** (cable to a switch that insists on 100): three drops → downshift at 20 s →
  **10 Mbps/Full steady** at 22 s → DHCP, time synced over Ethernet, 0 % loss.
- **MAC (patch 0017):** the label on the cover gives **eth0 = `2c:f7:f1:…`** (the SoC eFuse,
  Seeed OUI) and **"eth1" = `60:ff:9e:…`**, which is the **Wi-Fi** (CYW43012 OTP; BT uses
  that minus one). The vendor U-Boot only reads the eFuse with the `cvi_otp` command, which
  **saves the environment by itself**; without it U-Boot makes up a random, locally
  administered MAC and hands it to the kernel, which is why both the factory system and 7.2
  changed MAC at every boot. The `dwmac-cvitek` glue now takes the eFuse MAC
  (`0x03050140/144`, with `clk_efuse` on) when the DT carries an invalid or locally
  administered one.
- **Adapter pins:** the cable uses 4 wires, the internal PHY's pairs on the pads
  `ETH_TXP/TXM/RXP/RXM` (selector at `0x03001124–0x03001130`, function 3 today). These pads
  also have digital functions (`XGPIOB_24–27`, UART3, PWM12–15, SPI1, I2S2), but they are in
  the **`VDD18A` (1.8 V)** domain, unfit for 3.3 V peripherals without level shifting.
  Ethernet stays enabled and these pins are not reused.

---

## 6b. Wi-Fi HaLow (802.11ah), optional module — supported as on the factory system, not validated on hardware

- **Hardware:** a Morse Micro **MM6108** module ("fgh100m"), an add-on board that goes **in
  place of the SD card** (SDIO on `sdhci0`, `0x4310000`; reset by `portc 12`), plus an
  ADS1015 ADC on i2c1 `0x48`. It takes the slot, so it is used with the system on the eMMC.
- **Factory (5.10):** the out-of-tree Morse driver (release 1.14.1) built into the kernel by
  a patch; `loadsystemko.sh` loads `dot11ah.ko`, `mac80211.ko` and
  `morse.ko country=$COUNTRY` (default `US`) and, if `/dev/morse_io` exists, `ti-ads1015`;
  `S70hardware` renames the interface to `halow0`; userland is
  `wpa_supplicant_s1g`/`wpa_cli_s1g` and `morse_cli`; firmware
  `/lib/firmware/morse/{mm6108.bin,bcf_default.bin}`.
- **7.2:**
  - br2-external packages: `morse-driver` (vendor source at tag 1.14.1 +
    `0001-port-to-linux-7.2.patch`), `morse-cli` and `wpa-supplicant-s1g` (Morse's hostap
    fork, same tag);
  - the driver port: action frames (`action_code` moved into `u.action`), timers, GPIO
    descriptors, adapters for the `mac80211`/`cfg80211` callbacks (`radio_idx`, `link_id`,
    `link_sta`, `wireless_dev`), an S1G shim (`dot11ah/s1g_compat_new.h`: private width
    flags and the 5.10 short-beacon semantics);
  - kernel: patch 0019 (`MORSE_HALOW_DEPS`: selects `CRC7` and the ciphers),
    `TI_ADS1015=m`;
  - DT: `&sdhci0` accepts SDIO and gets the child `mm610x-sdio@2`
    (`reset-gpios = <&portc 12>`), as in the factory DT;
  - the factory firmware; an `S70halow` init with the factory sequence, which only loads
    the modules if a Morse SDIO card (`0x325b`) is in the slot.
- **Validation:** builds and installs (`updates/morse.ko`, `updates/dot11ah/dot11ah.ko`,
  `modules.dep` resolves `crc7`/`mac80211`/`dot11ah`). **Real operation needs the HaLow
  module, which we do not have.** Reports welcome.

---

## 7. Reboot and power off

- **Factory:** `drivers/power/reset/cvi-reboot.c` (`compatible "cvitek,restart"`,
  `reg 0x05025000`). It unlocks the RTC with `0xAB18`, asks for `RTC_EN_WARM_RST_REQ`
  (reboot) or `RTC_EN_SHDN_REQ` (power off) and writes `RTC_CTRL0`. It is built as
  **`obj-y`, without a Kconfig entry**, so an inventory of the port by `CONFIG_` symbols
  never sees it.
- **Symptom before:** `reboot` stopped at "Restarting system", and then not even the reset
  button worked; only a power cycle. The board's OpenSBI 0.9 offers no SRST.
- **7.2:** **patch 0009** (`CONFIG_POWER_RESET_CVITEK`): the same register sequence, the
  current `sys-off` API, bounded waits; a `restart-controller@5025000` node in the DT.
- **Validation:** `reboot` → FSBL again within 1.4 s → login, unattended.

---

## 7b. SoC temperature and thermal protection

**Why it matters:** on the Seeed system the board **rebooted after long periods with the
stream and object detection running**. The investigation showed that the Seeed thermal
protection was not enough, and that 7.2 had none at all.

**Chip limits** (SG2002 Datasheet V1.0, §2.5.2):

| Parameter | Value |
|---|---|
| Operating ambient | −30 to 70 °C |
| **Recommended** junction temperature | up to **85–105 °C** |
| **Destructive** junction temperature | **125 °C** (irreversible damage) |
| **DRAM** (inside the package) | guaranteed only up to **115 °C**: above it **memory contents may corrupt** |
| Thermal resistance | θJA 20.3 °C/W (no airflow, JEDEC 2s2p board), θJC 6.9, θJB 5.32 |

**What the factory system did** (5.10 + MPI):
- **Trip points** (`cv181x_base.dtsi`): passive at 100 and 110 °C, **critical at 130 °C**.
  The critical trip is **above the destructive limit (125 °C) and the DRAM limit
  (115 °C)**: before the shutdown acts, memory may already be corrupting.
- **`clock_cooling`** (the Seeed override in the reCamera DT):

  | State | CPU | NPU |
  |---|---|---|
  | 0 ("Max Performance Test") | 850 MHz | 700 MHz |
  | 1 | 500 MHz | 500 MHz |
  | 2 | 425 MHz | 400 MHz |

- **Frame rate:** the MPI's `tz_event_handler` thread (`cvi_mpi/modules/sys/src/cvi_thermal.c`)
  cut the camera rate to 15 → 5 → 1 fps if the application registered the callback.
- **Likely cause of the reboots:** long use in a closed case takes the junction above
  115 °C, the DRAM corrupts and the board hangs or resets, below the 130 °C critical trip,
  which therefore never acts.

**On 7.2:**
- **Mainline has no driver for the SG2002 temperature sensor:** no `thermal_zone`, no
  `cpufreq`, no critical shutdown and **no temperature-driven DDR refresh**.
- **Patch 0021** ports the Sophgo `cv181x_thermal.c` (`CONFIG_CV181X_THERMAL=y`, built in,
  active from boot):
  - reading `temp = res × 716 / 2048 − 273` °C, every second;
  - turns the **DDR refresh by temperature** back on (85/75 °C), as on the factory system.
- **New trip points, from the datasheet** (`sg2002-recamera.dts`):
  - `hot` at **85 °C**: an event for userspace;
  - `critical` at **110 °C**: emergency shutdown, 5 °C below the DRAM limit.
- **The cooling policy belongs to userspace** (the application), cutting in this order:
  inferences per second, frame rate, sensor binning mode, stream bitrate, LED.
- The OV5647 has no reachable temperature reading: heat shows as noise, not as an event.

**Reboot diagnostics:**
- `/etc/init.d/S03bootreason` → `/data/recamera/boot-history.log`: one line per boot with the
  **power-on reason** (`RTC_ST_ON_REASON` = `0x050260F8`), the slot and the last logged
  temperature. The raw value **does not match** the Sophgo table (`0x800d0000` cold,
  `0x880d0003` watchdog, `0x840d0003` warm reset); measured: software `reboot` =
  `0x04090003`, power-on = `0x000D0000`. It also copies the pstore log of a crash (§11) to
  `/data/recamera/crash/`.
- `/usr/sbin/thermal-logger` (`S04thermal-logger`) → `/data/recamera/thermal.log`:
  `epoch temp_mC load1 uptime zone` every 10 s, `sync` on every line (the curve survives a
  power loss), rotated at 1 MiB.
- **Measured:** 36 °C idle in the closed camera; ~52 °C with YOLO and the stream running.

---

## 7c. Camera: the OV5647 MIPI-CSI wiring

**The reCamera's sensor is wired to the SG2002 with the MIPI clock on physical lane 2 and
the data on lanes 0 and 3: `lane_id = {2, 0, 3}`.**

**Why this is documented nowhere:**
- The Sophgo sensor library (`SensorSupportList/sensor/cv181x/ov_ov5647/ov5647_cmos_param.h`)
  has `lane_id = {0, 1, 2}`. That structure is **byte for byte the same in the compiled
  library of the factory system** (`/mnt/system/usr/lib/libsns_ov5647.so`).
- The Seeed software (`sscma-node`) **overrides the mapping at runtime**, with no
  configuration file (there is no `sensor_cfg.ini` on the reCamera).
- **Consequence:** any program that uses the Sophgo MPI/sensor library as shipped, outside
  `sscma-node`, **never gets a frame**. The sensor takes the configuration and says it is
  streaming, but the receiver decodes nothing.

**Symptoms of the wrong mapping:** `/proc/mipi-rx` shows the data lanes in `hs_idle`, lane
D3 in `hs_err`, the clock lane state changing between runs; zero CSI/VI interrupts,
`VISofCh0Cnt = 0`; `CVI_VI_GetChnFrame` times out.

**How it was measured:** boot the factory SD card, open its web terminal (`ttyd`, port
9090), start the camera through `sscma-node` itself (over MQTT), read `/proc/mipi-rx`:
`LinkId 2, 0, 3`, with `D2 hs_sync`, `D3 hs_hst`, `CK_HS 1`. Details in `docs/multimedia.md`.

**On our system:** `external/package/sophgo-mpi/0002-ov5647-recamera-lane-mapping.patch`
changes the library default to `{2, 0, 3}`, so any MPI client works without knowing.
Validated: 1920×1080 frames with 3A running.

**Other camera wiring facts** (all confirmed): I²C2, address `0x36`; MCLK0 at 24 MHz
(23.76 MHz on 7.2, 1188/50); sensor power on `CAM_EN` = `PWR_GPIO_6` (gpio358), held high in
the DT; no reset GPIO; 10-bit RAW, MAC at 400 MHz, `hs_settle` 8, no polarity inversion
(`pn_swap` 0). **Orientation:** with the USB-C at the bottom the picture comes out upright,
neither mirrored nor flipped.

---

## 8. System time (no battery-backed RTC)

- **Factory:** only `ntpd` at boot. Without a network the system stays in 1970, and TLS
  certificates, WebSocket and MQTT fail.
- **7.2:**
  - `S01timestamp` (after `S01data`): restores the **last saved time** at boot and saves
    it at shutdown. The file is `/data/recamera/timestamp` on the eMMC (it survives slot
    updates) and `/var/lib/timestamp` when running from the SD card;
  - a `udhcpc` hook, `usr/share/udhcpc/default.script.d/50-timesync` →
    `usr/sbin/timesync-now`: **the moment any interface gets an address**, it stops
    `ntpd`, runs `ntpd -gq` (set now), saves the time and restarts `ntpd`
    (log `/var/log/timesync.log`).
- **Validation:** 1970 → the right time in the same second as the Wi-Fi `bound`; also via
  `bound:eth0`; after a `reboot` the boot starts at the last saved time.
- **Time zone:** `/etc/localtime` is a link to `/data/recamera/localtime`, set from the
  page (System); the tz database is in the image.

---

## 9. BSP coverage: how the missing pieces were found

The first inventory compared `CONFIG_` symbols. A sweep of the whole vendor tree found
three classes that method cannot see:

1. **`obj-y` without a Kconfig entry.** Example: `cvi-reboot.c`.
2. **Vendor changes inside standard drivers**, under `CONFIG_ARCH_CVITEK`. Examples:
   `dwc2`, `i2c-designware`, `stmmac`.
3. **Implicit behaviour of the vendor drivers**, in neither the configuration nor the DT.
   Examples: SDIO capped at 46.875 MHz, the ADC turning the I2S3 MCLK on, and the pin mux
   done by the vendor U-Boot.

---

## 10. Kernel patches (`external/patches/linux/7.2/`)

| Patch | What |
|---|---|
| 0001 | internal Ethernet PHY CV182XA (the vendor driver) |
| 0002, 0003 | `dwmac-cvitek` glue, PHY open order |
| 0004, 0005, 0007 | Ethernet clocks critical, `clk_efuse` published, Ethernet dividers |
| **0009** | reboot/power off through the RTC (`cvi-reboot`) |
| **0010** | ADC: `Capture Switch`, microphone muted at probe |
| **0011** | ADC: turns the I2S3 MCLK on (the ADC's clock source) |
| **0012** | I2S: accepts `set_sysclk(0)` (end of stream) without an error |
| **0013** | DMA: no "apb_regs not initialized" in fixed-channel mode (CV18xx) |
| **0014** | pinctrl: exclusive *bias* (pull-up clears pull-down) |
| **0015** | dwc2: buffer DMA (`g_dma = true`, `g_dma_desc = false`): 157/146 Mbit/s against 122/94 without DMA |
| **0016** | 8250_dw: DMA only when the DT describes `dmas` (UART1/BT in PIO, no warning) |
| **0017** | dwmac-cvitek: Ethernet MAC from the eFuse (the one on the label) |
| **0018** | cvitek PHY: plain genphy `read_status`, as the final factory kernel |
| **0019** | Kconfig anchor `MORSE_HALOW_DEPS` (selects `CRC7` and ciphers for the HaLow driver) |
| **0020** | driver core: `fw_devlink` at `dev_dbg` when it refuses a `SYNC_STATE_ONLY` link to a bound consumer |
| **0021** | **SG2002 temperature sensor** (`cv181x_thermal`, from Sophgo) + DDR refresh by temperature (§7b) |
| **0022** | **SG2002 PWM** (`pwm-cv1800`, new, from the logic of Sophgo's `cvi_pwm.c`): white LED 0–255 at 25 kHz (`PWR_GPIO0` → `PWM_8`) |
| **0023** | **SG2002 watchdog** (`dw_wdt` + `sophgo,cv1800-wdt`: sets the chip-reset bit at `TOP + 0x8`); the camera restarts by itself if it hangs |

---

## 11. Pitfalls

- **Touching the registers of a block whose clock is off hangs the whole SoC** (the bus
  never answers: no panic, no reboot, only a power cycle). Seen with the eFuse
  (`devmem 0x03050100+` with `clk_efuse` off) and with `vip_sys`. Mainline turns off at the
  end of boot every clock no driver asked for (`clk_disable_unused`), while the Sophgo
  kernel left many on (`CLK_IS_CRITICAL`/`CLK_IGNORE_UNUSED`). Rule: every ported driver
  enables its block's clocks before the first access. **Never read the eFuse with
  `devmem`.**
- **Cache invalidation on 7.2 RISC-V is `dma_sync_single_for_cpu`, not `_for_device`.** The
  Sophgo code invalidated with `arch_sync_dma_for_device(…, DMA_FROM_DEVICE)`, which on
  their 5.10 discarded the lines. On 7.2 the same call **only cleans**, and the invalidate
  is done by `arch_sync_dma_for_cpu`. Replacing the unexported function with
  `dma_sync_single_for_device(DMA_FROM_DEVICE)` leaves the invalidate without effect: the
  CPU reads stale cache, intermittently and with no error. Seen as corrupt H.264 (a PPS
  with the bytes of the SPS); it would hit the NPU outputs too. Rule:
  data from the hardware → `_for_cpu(DMA_FROM_DEVICE)`; data to the hardware →
  `_for_device(DMA_TO_DEVICE)`. Fixed in `sys` and in ION.
- **An encoder channel without `u32BufSize` hangs the SoC.** Without a size, the JPEG
  channel asks ION for 9 MiB; if that fails, the Sophgo error path hangs the SoC. Always
  set `u32BufSize`.
- **Crash logs:** a 1 MiB ramoops buffer at `0x8ff00000` keeps the kernel log of a panic or
  a hang across the watchdog reset; `S03bootreason` copies it to
  `/data/recamera/crash/<date>-<cause>/` at the next boot and clears pstore.
- Changed the board defconfig → `make sg2002_recamera_emmc_defconfig` before `make all`.
- A new or removed kernel patch → `make linux-dirclean`. DT only → `make linux-rebuild`.
  BusyBox configuration → `make busybox-dirclean`.

## 12. The boot log

With patches 0012–0016, the DT (`system-clock-frequency` of 12.288 MHz on the sound cards,
`mdio` with `ethernet-phy@0` and `phy-handle`, `snps,multicast-filter-bins = <64>`, one
*bias* per group) and `CONFIG_LOG_BUF_SHIFT=17` (the whole boot in the buffer), only 4
informational lines are left: `DMI: not present`, `efi: UEFI not found` (no UEFI on this
board) and `dwmac IRQ eth_wake_irq/sfty not found` (optional IRQs this MAC does not have).

## 13. Open

- HaLow: supported (§6b); needs the physical module.
- Why `wpa_supplicant` reading its configuration from `/data` makes the boot handshake slow (§2).

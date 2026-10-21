# recamera-linux-kernel7

**Linux 7.2 and a lean Rust application for the Seeed reCamera 2002 (Sophgo SG2002, RISC-V).**

A complete system for the reCamera built with Buildroot 2026.05.3: mainline Linux 7.2 with
the board's hardware ported driver by driver, the Sophgo multimedia stack (camera, ISP,
scaler, H.264/JPEG encoders, NPU) ported from their Linux 5.10 to 7.2, an A/B layout on the
eMMC, and one Rust daemon that does YOLO detection on the NPU and serves the video over
WebRTC, RTSP and MJPEG, a web page, MQTT with Home Assistant discovery, an optional
LLM/VLM call per detection, and a terminal in the browser.

## Why

The reCamera ships with Linux 5.10 and a stack of Seeed services (Node-RED, `sscma-node`,
a supervisor, an overlay filesystem). Seeed's support stopped there, and their effort
moved to a newer camera model. I wanted a current kernel and a smaller, more efficient
system I fully understand: one daemon instead of a dozen services, nothing running that
the camera does not need, every change reproducible from this tree.

When Seeed sent me the camera to evaluate, I asked them for exactly this — what the port
to a current kernel would take — and never found anything better than what ended up here.
Much of the hardware behaviour below is documented nowhere else, so the notes are written
for whoever tries the same on this board or on another SG200x (Milk-V Duo 256M, LicheeRV
Nano). The **pitfalls** section is the part most worth reading.

## What works

| | State |
|---|---|
| Linux 7.2, Buildroot 2026.05.3, musl, GCC 15.3 | ✅ |
| eMMC A/B slots (8 GiB each) + persistent `/data`, fallback to the other slot, SD-card installer | ✅ |
| Wi-Fi (CYW43012, SDIO), Bluetooth/BLE (same chip, UART1) | ✅ |
| USB-C network (NCM gadget, 192.168.42.1), Ethernet (internal PHY, adapter cable) | ✅ |
| Microphone and speaker (the SoC's internal codec) | ✅ |
| SoC temperature, thermal shutdown below the DRAM limit, watchdog, reboot/power off, crash log across resets (pstore) | ✅ |
| White LED with PWM brightness | ✅ |
| Camera (OV5647): MIPI, ISP with 3A, VPSS, hardware JPEG and H.264 | ✅ 30 fps |
| NPU (YOLOv8/YOLO11 `.cvimodel`) | ✅ ~25 fps at 1280×960 with the stream running |
| `recamerad`: web page, WebRTC, RTSP, MJPEG, MQTT + Home Assistant, LLM/VLM, browser terminal | ✅ |
| Wi-Fi HaLow (optional Morse Micro module) | ✅ ported, ⏳ untested (no module here) |
| Firmware update from the page, Wi-Fi onboarding without a cable, Bluetooth configuration | ⏳ |

Measured with everything on (detection, RTSP and MJPEG viewers): the daemon uses ~29 % CPU
and 7–17 MB of RSS, ION 26–28 of 36 MB, ~165 MB of RAM free.

## Quick start

You need Docker and ~20 GB of disk. Everything builds inside the container.

```sh
git clone --recurse-submodules https://github.com/djairjr/recamera-linux-kernel7.git
cd recamera-linux-kernel7
docker build -t recamera-build:latest container/          # the toolchain image, ~18 min
container/run.sh make sg2002_recamera_emmc_defconfig
container/run.sh make all                                   # an hour or more the first time
```

The result is `output/sg2002_recamera_emmc/images/sdcard.img`. Optionally put your Wi-Fi
network in `local/wpa_supplicant.conf` before building (it is not versioned) and the image
joins it at the first boot.

**Run from the SD card:** write the card (on Linux:
`pkexec bash "$PWD/scripts/write-sd-image.sh" "$PWD/output/sg2002_recamera_emmc/images/sdcard.img" /dev/sdX`,
any raw writer works), put it in the camera, power on. With a card inserted the boot ROM
always starts from the card, so this is also the recovery path.

**Install to the eMMC** (this erases the factory system; keep the factory recovery image
from Seeed if you may want it back): boot the card, log in over USB-C
(`ssh recamera@192.168.42.1`), become root (`su`) and run `emmc-install` to see what it
will do, then `emmc-install --yes`. Power off, remove the card, power on.

**Use it:**
- page: `http://192.168.42.1` over USB-C, or the camera's Wi-Fi address; user `recamera`,
  password `recamera` — **change it on the System page** (it changes the system password,
  ssh included);
- RTSP: `rtsp://recamera:<password>@<camera>/live`;
- models: the image ships no weights (Ultralytics models are AGPL-3.0). Upload a
  `.cvimodel` for cv181x with its `.json` on the YOLO page, or extract the factory ones
  with `scripts/models.sh` from the official Seeed image;
- updates: `scripts/slot-update.sh --switch` writes a new build to the inactive slot over
  ssh and boots it; the previous slot stays as it was.

More in `docs/build-environment.md`.

## Repository

```
container/      Docker image of the toolchain (Buildroot-built riscv64/musl GCC 15.3, patched)
buildroot/      Buildroot 2026.05.3 (submodule)
external/       the br2-external tree: defconfig, board files (DT, kernel config, rootfs
                overlay, genimage, post-build), kernel patches, packages
  package/sophgo-*     Sophgo ports to 7.2: ion, osdrv (kernel modules), mpi, cviruntime
  package/recamera-*   boot firmware, firmware blobs, the application, test programs
  package/morse-*      Morse Micro HaLow driver and tools
app/            the Rust workspace (recamerad and friends)
scripts/        deployment, port editing, models, diagnostics
tests/          small C check programs of the port
docs/           the documentation (below)
```

| Document | Contents |
|---|---|
| `docs/hardware.md` | the board, block by block: factory behaviour, what 7.2 needed, measurements, kernel patches |
| `docs/multimedia.md` | the Sophgo stack on 7.2: ION, sys/base, cif/vi, VPSS, codecs, NPU; cache, `fence.tso`, process cleanup, memory sizing |
| `docs/emmc-layout.md` | eMMC partitions, A/B, U-Boot environment, the SD installer, the boot firmware |
| `docs/application.md` | `recamerad`: architecture, configuration, API, MQTT topics, LLM |
| `docs/build-environment.md` | the container, Buildroot, rebuild rules, scripts |
| `docs/vendor-pins-0.2.4.md` | the reCamera-OS 0.2.4 commits the vendor sources are pinned to |

## How the system is put together

- **Boot:** the SG2002 ROM loads `fip.bin` (FSBL with DDR init → OpenSBI → U-Boot) from the
  eMMC boot partition, or from the card's FAT when a card is inserted. U-Boot reads its
  environment from the eMMC (8 MiB offset, outside every partition) and boots
  `boot-<slot>.itb` (a FIT: gzip kernel + DT) with `root=/dev/mmcblk0p2|3`. If the slot's
  kernel is missing it boots the other slot with `recamera.fallback=1`.
- **eMMC:** MBR; p1 FAT32 with both kernels, p2/p3 the two 8 GiB root filesystems, p4
  `/data` (everything that survives updates: configuration, passwords, Wi-Fi networks,
  models, logs, crash reports).
- **Kernel:** mainline 7.2 plus 22 patches (`external/patches/linux/7.2/`), the board DT
  (`external/board/.../dts/`), and out-of-tree modules for ION, the Sophgo media drivers
  and HaLow.
- **Userspace:** BusyBox init scripts in the rootfs overlay; `recamerad` started by
  `S90recamerad`; the Sophgo MPI libraries; `wpa_supplicant`, BlueZ, `dropbear`.

## Pitfalls and findings

What cost the most time, roughly in the order a port meets it. Each one has the full story
and the measurements in the docs.

**Toolchain and firmware**
1. **The C906 traps on `fence.tso`.** GCC 15 emits it for acquire-release fences (60 times
   in `libstdc++`); the T-Head C906 raises an illegal instruction instead of running it as
   the `fence rw,rw` its encoding falls back to. Programs die with SIGILL at random places
   (we met it at process exit inside `libcviruntime`). Fixed twice: GCC patched to not emit
   it, and the OpenSBI in the fip patched to emulate it (0.32 µs each) for code we did not
   compile. `docs/multimedia.md` §6.2.
2. **Do not touch a block whose clock is off.** Mainline turns off every clock no driver
   claimed (`clk_disable_unused`); the Sophgo kernel left them on. A register access to a
   gated block hangs the whole SoC — no panic, no watchdog, only a power cycle. Every ported
   driver must enable its clocks first (`base`, `tpu`, the CSI clocks, the eFuse). Never
   `devmem` the eFuse.
3. **The FSBL locks two eFuse bits** (`lock_efuse_chipsn`) at every boot if they are not
   set yet — on any camera that ever booted the factory firmware they already are.
4. **Out-of-tree modules are not rebuilt with the kernel.** A config option
   (`DETECT_HUNG_TASK`) changed `struct semaphore`, the old modules kept loading and
   corrupted memory. Rebuild them together and turn on `CONFIG_MODVERSIONS`.

**Board hardware**
5. **Wi-Fi: SDIO at 50 MHz kills 2.4 GHz channels 5–7.** Mainline's SDIO host reaches exactly
   50 MHz (the vendor's never passed 46.875); the bus radiates into the radio next to it, and
   an AP on channel 6 vanishes. `max-frequency = <37500000>` on `sdhci1`. Reference for any
   radio problem: the factory SD card in the same position.
6. **The camera's MIPI lanes are `{2, 0, 3}`** — clock on physical lane 2. The Sophgo sensor
   library says `{0, 1, 2}` (also in the factory binary); Seeed's `sscma-node` overrides it
   at runtime, so any other MPI program gets **zero frames** with no error. Patched into the
   library default.
7. **Sensor width must be a multiple of 64** for the SG2002 ISP: the mainline 1296×972
   binned mode streams but never finishes post-processing; 1280×960 works and gives the whole
   field of view at 4:3.
8. **Audio:** the ADC has no clock of its own — it runs on the **I2S3 MCLK of the speaker
   path**. Without it the microphone records random full-scale noise. The mic also starts
   muted, because volume 0 is not silence.
9. **Bluetooth** is on the Wi-Fi chip, over UART1 with flow control, loaded with
   `brcm_patchram_plus`; mainline `hci_bcm` has no 43012 entry.
10. **Reboot was an `obj-y` driver with no Kconfig entry** (`cvi-reboot`), invisible to a port
    that inventories `CONFIG_` symbols; without it `reboot` hangs and even the reset button
    stops working. The watchdog then registers a higher-priority restart handler; adjust the
    priorities or every reboot becomes a watchdog reset.
11. **Ethernet MAC:** the vendor U-Boot only reads the eFuse MAC with a command that also
    saves the environment; otherwise it hands the kernel a random one. The 7.2 glue reads the
    eFuse itself. The adapter cable does not sustain 100BASE-TX: an init script falls back
    to 10 Mbps only when 100 keeps dropping.
12. **Thermal:** the factory critical trip is **130 °C**, above the chip's destructive limit
    (125 °C) and the in-package DRAM limit (115 °C, contents may corrupt). Likely the cause of
    the reboots seen after hours of streaming. 7.2 has a ported sensor driver with `hot` 85 °C
    and `critical` 110 °C, plus the temperature-driven DDR refresh the factory system had.
13. **USB gadget DMA:** descriptor DMA breaks TCP from the camera (the SSH banner never
    arrives); buffer DMA works and is faster than PIO (157/146 Mbit/s vs 122/94).
14. **No RTC battery:** the clock restarts in 1970, and TLS, MQTT and certificates fail. The
    last known time is saved in `/data` and restored at boot, and NTP runs the moment any
    interface gets an address.

**Multimedia stack**
15. **Cache invalidation changed meaning.** On 7.2 RISC-V, `sync_for_device(FROM_DEVICE)` only
    *cleans*; the invalidate is `sync_for_cpu`. Translating the Sophgo calls the obvious way
    makes every invalidate a no-op: the CPU reads stale data, intermittently, with no error
    (we saw an H.264 PPS containing the SPS; NPU outputs would be silently wrong).
16. **ION is gone from mainline** (5.11); the MPI talks to `/dev/ion`. ION is ported as a
    module with the same ABI and the reserved region found with `of_reserved_mem_lookup()`.
17. **The H.264 encoder derives the profile from the entropy coder**: asking for Baseline
    gives Main unless `CVI_VENC_SetH264Entropy()` sets CAVLC. Constrained Baseline is what
    every WebRTC client must accept.
18. **An encoder channel without `u32BufSize` asks for 9 MiB** and, when ION is short, the
    Sophgo error path hangs the SoC.
19. **`kill -9` leaks ~20 MB of ION and can hang the SoC.** The drivers do not free MPI objects
    when the owner dies, and the kernel closes the dead process's files in fd order — VI
    before VPSS — stopping the ISP under a running scaler. Fixed with two small osdrv patches,
    a reclaim step at start, and the VI fd duplicated above 900 so it closes last.
20. **VI→VPSS online** removes the 9 MB VI frame pool; **4 VPSS blocks per pool**, not CPU, was
    what kept both the stream and YOLO at full rate. 36 MiB of ION is enough with room for a
    bigger model.

**System**
21. **The vendor U-Boot does pin muxing Linux relies on** (camera enable, camera I2C, ADC I2C,
    Bluetooth UART, LEDs, SD power): replacing the bootloader means moving that into the DT
    first (§ Boot firmware).
22. **FAT12 boot partitions fragment**: the vendor's 8 MiB boot partition cannot hold two
    kernels, and replacing the FIT in place made U-Boot fail with `Invalid FAT entry`. Ours is
    256 MiB FAT32.
23. **`wpa_supplicant` reading its configuration from `/data` made the boot handshake take
    minutes**; from `/etc` (restored from `/data` before it starts) it takes seconds. Cause
    unknown.
24. **The power-on reason register** does not match the Sophgo table; calibrated:
    `0x000D0000` power-on, `0x04090003` reboot, `0x08090003` watchdog.
25. **A ramoops buffer at the top of DRAM keeps the kernel log across the watchdog reset**
    (the reset is warm): that is how the `kill -9` hang above was found without a serial
    console.

## Boot firmware

`fip.bin` is **checked in**, not built: `external/package/recamera-fip/fip.bin` is the
official Seeed fip with only its OpenSBI replaced (vendor OpenSBI 0.9 + the `fence.tso`
emulation), byte for byte the one on the camera's eMMC. `make-fip.sh` next to it rebuilds
it from the vendor SDK and checks that nothing else changed. Its pieces and their sources
and licenses are listed in that package's `README.md`.

A fip fully from sources was tried (21/10/2026): the FSBL from `sophgo/fsbl` builds identical
to Seeed's, and repacking Seeed's FSBL + OpenSBI + U-Boot **without** the second core's
FreeRTOS boots fine (which removes an overlap between FreeRTOS and the ION/ramoops memory).
Mainline U-Boot 2026.04 did not start on this board, and the camera has no serial console
once assembled, so it was left for later. Details in `docs/emmc-layout.md` §9.

## Licenses

- Board support, scripts, kernel patches and the Buildroot external tree: **GPL-2.0**
  (`LICENSE`).
- The Rust application (`app/`): **MIT OR Apache-2.0** (`app/LICENSE-MIT`,
  `app/LICENSE-APACHE`).
- Third-party sources are downloaded by their packages at pinned commits, under their own
  licenses (Linux GPL-2.0; Sophgo kernel drivers GPL; Sophgo MPI and runtime, Morse Micro,
  hostap: see each package's `_LICENSE`). Prebuilt firmware (Wi-Fi/BT, sensor tuning, video
  codec microcode, the boot fip) is redistributed as shipped by the vendors.
- xterm.js (`app/recamerad/web/vendor/`): MIT.

## Acknowledgements

Sophgo for publishing their SDK sources; Seeed Studio for the hardware and the reCamera-OS
tree this port reads from; the mainline Linux and U-Boot work on the CV18xx/SG200x family;
the Armbian SG200x OpenSBI patch; Morse Micro's open HaLow driver; Buildroot.

This project is not affiliated with Seeed Studio or Sophgo. Writing the eMMC erases the
factory system; you do it at your own risk.

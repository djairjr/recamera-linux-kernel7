# eMMC layout, A/B slots and the SD recovery card

**State:** installed and validated on the camera on 18/10/2026 (§8); in daily use since.

## 1. What the camera ships with (measured 18/10/2026)

**The chip**
- Samsung CJTD4R (`manfid 0x15`), made 12/2022, eMMC 5.1 (`EXT_CSD_REV` 8).
- 122,142,720 sectors × 512 B = 62.5 GB (the "64 GB" model; `lsblk` shows 58.2 GiB).
- Health: `life_time 0x01 0x01` (0–10 % used), `pre_eol_info 0x01` (normal).
- Runs at HS200 with 64-bit ADMA under Linux 7.2.
- Hardware areas: `boot0` and `boot1` (4 MiB each), `rpmb` (4 MiB).
- `EXT_CSD[179]` (`PARTITION_CONFIG`) = `0x08`: the ROM boots from boot0.
- A full read of the 58.2 GiB finished without an I/O error in 2095 s.

**The vendor user area has no partition table.** The Seeed scheme uses fixed offsets:
- U-Boot reads `BOOT_PART_OFFSET` and `BOOT_PART_SIZE` from its environment;
- the kernel gets the map on its command line:
  `blkdevparts=mmcblk0:8192K(BOOT),128K(ENV),1310720K(ROOTFS),524288K(ROOTFS2),262144K(RESERVED),-(USERDATA)`;
- the real ROOTFS is 1.25 GiB, not the 512 MiB of the SDK's `partition_emmc.xml`.

**Who boots:**
- with a card in the slot, the ROM loads `fip.bin` from the card's FAT;
- without a card, it loads the fip from `boot0`;
- the U-Boot environment is **always on the eMMC** (offset 8 MiB, 128 KiB), even when
  booting from the card.

## 2. Constraints

- **U-Boot:** the vendor one, inside the fip (`external/package/recamera-fip`: the
  official fip with only its OpenSBI replaced, see `docs/build-environment.md` §5).
  - Its environment is fixed on the eMMC at 8 MiB (`ENV_PART_OFFSET=0x4000` sectors,
    128 KiB). That range **must stay outside every partition**.
- **What the vendor U-Boot was seen to read:** MBR (DOS) tables, FAT (`fatload`) and ext4
  (`ext4load`), all proven on the SD card. Nothing in the binary suggests GPT support.
- **Linux 7.2:** `MSDOS_PARTITION=y`, `CMDLINE_PARTITION=y`; ext4 and vfat built in.
- **Goals:** none of the Seeed overlay scheme; a bigger rootfs; **two complete root
  filesystems (A/B)**, the other one as a fallback to recover from a broken update.
- **Lesson of 18/10:** the vendor's 8 MiB FAT12 boot partition cannot hold two copies of
  the FIT (4.7 MB). Replacing the file in place fragments it, and U-Boot fails with
  `Invalid FAT entry`.

## 3. Layout (MBR, 1 MiB aligned)

| Region | Start sector | Size | Contents |
|---|---|---|---|
| MBR | 0 | 512 B | DOS table |
| U-Boot environment | 16384 (8 MiB) | 128 KiB | outside the partitions, where the vendor U-Boot expects it |
| **p1 BOOT** (FAT32, `BOOT`) | 32768 (16 MiB) | 256 MiB | `boot-a.itb`, `boot-b.itb` |
| **p2 ROOT_A** (ext4, `rootfs-a`) | 557056 | 8 GiB | system A |
| **p3 ROOT_B** (ext4, `rootfs-b`) | 17334272 | 8 GiB | system B |
| **p4 DATA** (ext4, `data`) | 34111488 | the rest (~41 GiB) | `/data`, shared by both slots |

- **boot0** (hardware area): `fip.bin` at 0 and at 1 MiB, like the vendor's `fip`/`fip_bak`.
- **boot1:** another copy of the same `fip.bin`.

**Why:**
- **MBR:** self-describing partitions, which the vendor U-Boot already reads.
- **Four primaries:** they fit the MBR exactly, no extended partition.
- **A 256 MiB FAT32 p1:** no more FAT12 fragmentation.
- **One kernel per slot:** the kernel modules live in the rootfs, so kernel and rootfs
  travel together.
- **DATA survives reinstalls** (when it is already ext4, labelled `data`, same start).

## 4. The U-Boot environment

Built from `board/.../uboot-env-emmc.txt` with `mkenvimage -s 0x20000` (measured format:
CRC32 + data, no redundancy flag) and written at 8 MiB by the installer.

- `bootcmd=run sdboot || run emmcboot || run emmcboot_other`
- **`sdboot`:** the card first. Inserting a card is still the recovery.
- **`emmcboot`:** the `slot` variable (`a`/`b`) picks `boot-${slot}.itb` and
  `root=/dev/mmcblk0p2|p3`; the kernel gets `recamera.slot=`.
- **`emmcboot_other`:** if the slot's kernel is missing, try the other one, with
  `recamera.fallback=1` on the kernel command line.
- **Dropped from the vendor environment:** `cvi_update`, `cvi_otp`, `blkdevparts`.
- **From Linux:** `fw_printenv`/`fw_setenv` (`/etc/fw_env.config`:
  `/dev/mmcblk0 0x800000 0x20000`). Switching slots by hand: `fw_setenv slot b`.
- `scripts/slot-update.sh` writes the build to the inactive slot over ssh, checks it,
  resizes it and, with `--switch`, sets `slot` and reboots.

## 5. The SD card (recovery and installer)

The build's `sdcard.img` has three partitions:

```
p1 boot     FAT 64 MiB    fip.bin + boot.sd             (the vendor's was 8 MiB FAT12)
p2 rootfs   ext4 200 MiB  the system, with /usr/sbin/emmc-install
p3 INSTALL  FAT32 320 MiB fip.bin, boot.itb, rootfs.ext4, uboot-env.bin, VERSION, SHA256SUMS
```

`emmc-install` (without arguments it only says what it would do; `--yes` runs it):

1. Needs root, the system running from the card, nothing of the eMMC mounted, and
   `SHA256SUMS` checked.
2. Writes the fip to boot0 (0 and 1 MiB) and boot1, `force_ro` 0→1, reading every write back.
3. Writes the MBR with `sfdisk` (`--wipe never`) and rereads the table.
4. Writes the U-Boot environment at 8 MiB and reads it back.
5. p1: `mkfs.fat -F 32`, `boot-a.itb` and `boot-b.itb`, read back.
6. p2 and p3: `blkdiscard`, `dd` of `rootfs.ext4`, read back, `e2fsck`, `resize2fs` to
   8 GiB, a label and a UUID of its own.
7. p4: kept, or created with `mkfs.ext4 -L data` (`--wipe-data` forces it); creates
   `/data/recamera` and `/data/models` and keeps the log there.

`--no-fip` skips step 2.

**On the installed system** `S01data` mounts `/dev/mmcblk0p4` on `/data` (after an
`fsck`) when root is `mmcblk0p2` or `mmcblk0p3`.

## 6. Updating

- **The whole system:** `scripts/slot-update.sh` to the inactive slot, `--switch`, test,
  then the other slot. The previous slot stays as it was until then.
- **From scratch, or a broken eMMC:** the card and `emmc-install --yes`.
- **Pending:** a boot counter in the environment with a confirmation from the running
  system, to fall back to the previous slot by itself; and the update from the web page
  (System section).

## 7. What lives in `/data/recamera`

Everything that must survive a slot update: `config.json` (the daemon), `passwords`
(restored into `/etc/shadow` at boot by `S02passwords`), `wpa_supplicant.conf` (copied to
`/etc` before `wpa_supplicant` starts), `localtime`, `timestamp` (the last known time:
the board has no RTC battery), `boot-history.log`, `thermal.log`, and `crash/` (the kernel
log kept by pstore across a watchdog reset).

## 8. Installation and validation (18/10/2026)

**Installation:** `emmc-install` ran from the card and ended with exit 0. Every write was
read back and matched: the fip in boot0 (+0, +1 MiB) and boot1, the MBR, the environment,
p1, p2 and p3 grown to 8 GiB, p4 created with 41 GiB. Two script bugs showed up in the dry
run and were fixed before writing: a missing `+x`, and `stat` missing from busybox (now
`wc -c`).

| Test | Result |
|---|---|
| Boot **without a card** | `Boot from eMMC slot a`, `root=/dev/mmcblk0p2`; the 41 GiB `/data` mounted by `S01data` |
| `fw_setenv slot b` + reboot | 4 of 4 cycles in `slot=b`, `root=/dev/mmcblk0p3`; back to `slot a`: correct |
| **Fallback** (with `boot-a.itb` hidden) | `** Unable to read file boot-a.itb **` → `slot kernel missing, trying slot b` → up on B with `recamera.fallback=1`; `slot` stays `a` |
| System running from the eMMC | Wi-Fi `COMPLETED`, BT `UP RUNNING`, NTP time, no new `dmesg` warnings, 220 MB of RAM available |

**Unexplained:** on the **first** reboot after `fw_setenv slot b` the camera came up on A
(no serial capture of that boot). It did not happen again in the next 5 captured boots;
since then a fallback shows as `recamera.fallback=1` in `/proc/cmdline`.

## 9. The fip from sources (tried, not adopted)

The vendor fip has four pieces:
- **BL2:** the FSBL, with the `ddr3_1866_x16` DDR init built in (`DDR_PARAM` empty);
- **BLCP_2ND:** FreeRTOS for the second core (C906L);
- **MONITOR:** OpenSBI;
- **LOADER_2ND:** U-Boot, LZMA-compressed, behind a 32-byte header (`B3MA`, checksum,
  size, run address; the FSBL jumps to run address + 0x20).

The board configuration is in reCamera-OS `external/build/boards/cv181x/sg2002_recamera_emmc/`:
`memmap.py`, the U-Boot configuration and `cvi_board_init.c`, the DT and the partition XML.

**What was reached:**
- **BL2** from `sophgo/fsbl` 92735b9 (T-Head toolchain of the SDK,
  `CHIP_ARCH=cv181x DDR_CFG=ddr3_1866_x16 STORAGE=emmc TPU_PERF_MODE=y LOG_LEVEL=2`):
  **identical** to Seeed's except 16 bytes (the date and git version in its version string).
- **Packing** with the `sophgo/fiptool` layout (BSD-2), `BLCP_2ND_RUNADDR` = 0 when there
  is no RTOS: Seeed's BL2 + OpenSBI 0.9 + U-Boot repacked this way, **without FreeRTOS**,
  boots the camera (21/10/2026; `0x8FE00000` reads zero afterwards, where the vendor fip
  leaves FreeRTOS code).
- **Mainline U-Boot 2026.04** (`sipeed_licheerv_nano_defconfig` + a reCamera DT with the
  eMMC as mmc 0 and the SD as mmc 1, the environment at the same eMMC offset, U-Boot kept
  below the ION carveout and ramoops): **did not start** on this board, with either OpenSBI
  1.7 or the vendor 0.9, with or without reading the environment (six SD-card tests, no
  serial console; a file it should have written to the card at the end of its init never
  appeared). The cause is open.
- **Found while doing it:** the vendor U-Boot's `cvi_board_init.c` sets the pin mux that
  Linux relies on today: `CAM_EN` (PWR_WAKEUP0 → PWR_GPIO_6), the camera I2C2
  (PWR_GPIO1/2), the ADC I2C1 (MIPI_TXM2/TXP2), Wi-Fi/BT UART1 and `REG_ON` (AUX0 →
  GPIOA 30), the LEDs (SPK_EN → GPIOA 15, GPIO_ZQ → PWR_GPIO_24, PWR_GPIO0), the SD pins
  and `SD0_PWR_EN` (→ GPIOA 14). **Any other bootloader needs these as pinctrl in the
  kernel DT first.**
- The FSBL (vendor and ours alike) calls `lock_efuse_chipsn()` at every boot: it sets two
  eFuse lock bits for the chip serial number **only if they are not set yet**. On a camera
  that has booted the vendor firmware once they already are.

**The second core and Linux memory:**
- The vendor fip loads FreeRTOS at **0x8FE00000–0x8FFFFFFF** (`CVIMMAP_FREERTOS_ADDR`,
  2 MB) and starts the C906L. Measured: `csrr a0, mhartid` (`0xF1402573`) at 0x8FE00000.
- The 7.2 DT does not reserve that range; it falls inside the ION carveout
  (`8dafe000–8fefffff`) and the ramoops buffer (`8ff00000`).
- Nothing went wrong so far: ION allocates from the bottom and its peak (26 MB of 36) never
  reached the last megabyte. It is still a latent cross-core corruption risk.
- **The fix**, proven by the packing test above: a fip without `BLCP_2ND`. Until it is
  installed, the ION peak must stay below ~34 MB.

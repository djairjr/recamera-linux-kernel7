# recamera-fip: the boot firmware (`fip.bin`)

The SG2002 boot ROM loads `fip.bin` from the eMMC boot partition (or from the
FAT partition of an SD card) and runs the chain inside it:

| Piece | What | Origin | License |
|---|---|---|---|
| BL2 | FSBL with the DDR init (`ddr3_1866_x16`) | Sophgo [fsbl](https://github.com/sophgo/fsbl), as shipped by Seeed | BSD-3-Clause |
| BLCP_2ND | FreeRTOS for the second core (C906L) | Seeed reCamera-OS 0.2.4 | MIT |
| MONITOR | OpenSBI 0.9 (`fw_dynamic`) **+ fence.tso emulation** | Sophgo [opensbi](https://github.com/sophgo/opensbi) + `0001-lib-sbi_illegal_insn-add-emulation-for-fence-tso.patch` | BSD-2-Clause |
| LOADER_2ND | U-Boot 2021.10 (LZMA) | Sophgo [u-boot-2021.10](https://github.com/sophgo/u-boot-2021.10) + Seeed [reCamera-OS](https://github.com/Seeed-Studio/reCamera-OS) board files | GPL-2.0+ |

Files:

- `fip.bin`: what the build installs (sha256 `2db62808…efadaf`, checked by
  `recamera-fip.mk`). It is byte for byte the one in the camera's eMMC.
- `fip-seeed-original.bin`: the official fip of reCamera-OS 0.2.4, the input.
- `make-fip.sh`: how `fip.bin` was made. It rebuilds the vendor OpenSBI with the
  vendor (T-Head) toolchain, checks that the unpatched rebuild is byte-identical
  to the official MONITOR, repacks, and checks that only MONITOR and two
  checksums change. It needs a reCamera-OS checkout (`SDK=`), so the build uses
  the checked-in result instead.

Why the OpenSBI patch: the C906 traps on `fence.tso` (illegal instruction)
instead of executing it as the `fence rw,rw` its encoding falls back to, and
current compilers emit it. Upstream OpenSBI 1.7 carries the same emulation.
See `docs/multimedia.md` and the top README.

Not done (yet): a fip from sources with mainline U-Boot. The top README
("Boot firmware") lists what was tried on 21/10/2026 and what it needs first.

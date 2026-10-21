# The Sophgo multimedia stack on Linux 7.2 (camera, ISP, VPSS, codecs, NPU)

Mainline Linux has a driver for none of the SG2002 multimedia blocks. This document is how
the Sophgo stack (kernel modules from `osdrv`, the ION allocator, the MPI userspace
libraries, the NPU runtime) was ported from their Linux 5.10 to 7.2, step by step, with
every problem found and what it taught.

```
OV5647 → MIPI-CSI (cif) → VI + ISP ─(online)→ VPSS ─┬─ ch0 640×640 RGB → NPU (TPU, YOLO .cvimodel)
                                                    └─ ch1 1280×720 NV21 ─┬─ H.264 (vcodec) → RTSP / WebRTC
                                                                          └─ JPEG           → MJPEG / snapshots
```

Nothing from the Seeed software stack is used. From Sophgo only what the pipeline needs.
The one Seeed file kept is **data**: the ISP tuning of the OV5647 (Sophgo has none for this
sensor).

## 1. What the factory system loads

`loadsystemko.sh` (cv181x): `sys`, `base`, `rtos_cmdqu`, `fast_image`, `cvi_mipi_rx`
(cif), `snsr_i2c`, `vi`, `vpss`, `dwa`, `vo`, `mipi_tx`, `rgn`, `clock_cooling`, `tpu`,
`vcodec`, `jpeg`, `cvi_vc_driver` (`MaxVencChnNum=9 MaxVdecChnNum=9`) and `ive`.

Factory memory map (`memmap.py`): ION 60 MiB, FreeRTOS on the C906L 2 MiB, boot logo
1.8 MiB; ~190 MiB left for Linux.

## 2. What was ported, what was left out

| Module | Lines (cv181x) | What for | Decision |
|---|---|---|---|
| ION (Android staging + Cvitek heap) | 3,885 | the video buffer allocator (`/dev/ion`) used by `sys`, `vi`, `tpu` | **ported as an out-of-tree module**, same ABI (§4) |
| `sys`, `base` | 1,374 + 5,234 | the MPI core: VB buffer pools, binds between blocks | ported |
| `cif` (`cvi_mipi_rx`) + `snsr_i2c` | 9,333 + 494 | MIPI-CSI receiver, sensor I²C | ported |
| `vi` (with the ISP) | 23,812 | capture and ISP | ported |
| `vpss` | 23,629 | scaling and colour conversion; feeds the NPU and the encoders | ported |
| `tpu` | 2,769 | the NPU | ported |
| `vcodec` + `cvi_vc_drv` | 84,126 | H.264/H.265, JPEG | ported (encoders only in use) |
| `jpeg` | 1,950 | JPEG | ported |
| `vo`, `mipi_tx` | 7,023 | display output | **out**: no display, the "screen" is the stream |
| `dwa`, `rgn`, `ive` | — | lens dewarp, OSD, classic vision | out |
| `fast_image`, `rtos_cmdqu` | 2,256 | fast camera on the C906L with FreeRTOS | **out** |
| `clock_cooling` | 349 | thermal | replaced by the thermal driver + userspace policy |

Every Sophgo kernel driver is `MODULE_LICENSE("GPL")`.

## 3. The hard parts, up front

1. **ION.** Mainline removed ION in 5.11 in favour of DMA-BUF heaps, and the MPI libraries
   talk to `/dev/ion` by ioctl. Porting ION as a module with the same ABI keeps userspace
   unchanged; the cost is in the `dma-buf` API changes (`iosys_map`, reservation locks).
2. **The kernel API from 5.10 to 7.2**, across ~150,000 lines (84,000 in the codec alone):
   `remove()` returning `void`, `class_create` without `owner`, timers, `dma-buf`/`iosys_map`,
   `of_*`, `proc_ops`, `vm_flags`, `get_user_pages`, semaphores, `set_fs` gone.
3. **Device tree.** The `cif`, `vi`, `vpss`, `tpu`, `vcodec`, `jpeg`, `sys`, `base` and ION
   nodes come from the factory `cv181x_base.dtsi`, with **clocks remapped** to the IDs of
   the mainline `clk-cv18xx` driver (they differ from the Cvitek ones) and resets by index.
   They live in `dts/sophgo/sg2002-recamera-multimedia.dtsi`.
4. **Closed ISP 3A.** Auto exposure, white balance and focus come as prebuilt `.o` files in
   `cvi_mpi`, also for `riscv64-unknown-linux-musl`, so they link with our musl toolchain.
5. **ISP tuning for the OV5647:** only the Seeed file exists
   (`isp_tuning/cv181x/src/ov_ov5647/ov_ov5647_sdr.bin`), used as calibration data.

## 4. Kernel side, step by step

Packages: `sophgo-ion` (ION), `sophgo-osdrv` (everything else, one `Kbuild` of ours instead
of the Sophgo Makefiles, `-Werror`, `0001-port-to-linux-7.2.patch`). The modules are
loaded at boot by `S59camera`.

### 4.1 ION (`cvi_ion.ko`)

The staging ION of the Sophgo 5.10 tree (core + carveout heap + Cvitek layer), out of tree.
Port points (each marked "7.2 port" in the code):
- `dma_buf_ops.vmap/vunmap` with `iosys_map`; shrinker with
  `shrinker_alloc`/`shrinker_register`; `close_fd` for `ksys_close`; `strscpy` for
  `strncpy`; no `force_uaccess`; `remove()` returning `void` (without the `kfree` of `devm`
  memory, a latent double free); `MODULE_IMPORT_NS("DMA_BUF")`; a local copy of
  `plist_add` (not exported); `dentry_path_raw`;
- cache maintenance through the DMA API on the ION device (the `soc` is
  `dma-noncoherent`) instead of the unexported `arch_sync_dma_for_device`: flush =
  `dma_sync_single_for_device(DMA_TO_DEVICE)`, **invalidate =
  `dma_sync_single_for_cpu(DMA_FROM_DEVICE)`** (§6.1);
- the reserved region found with `of_reserved_mem_lookup()` instead of the boot hook
  `RESERVEDMEM_OF_DECLARE("ion-region")`, which cannot exist in a module;
- one `module_init`, no `module_exit` (a permanent module, as it was built in).

Test: `tests/f7/iontest.c` follows the path of `cviruntime` and the MPI (query the heaps,
allocate, `mmap` the dma-buf, pattern intact after flush/invalidate, heap full at 32 × 1 MiB
with a proper `ENOMEM`, all memory returned).

### 4.2 `sys` and `base`

- Port: `strscpy`, `pde_data`, `class_create(name)`, `const` sysfs callbacks, `UTS_VERSION`
  from `<generated/utsversion.h>`, `remove()` → `void`, no `security_task_getsecid`, cache
  through the DMA API, `MODULE_IMPORT_NS("DMA_BUF")`; the same `CONFIG_ION_*` as
  `sophgo-ion`, **mandatory** because they change the layout of `struct ion_buffer`.
- **Removed on purpose:** the eFuse sysfs files of `base`, including eFuse **writing**
  (irreversible).
- **Hang found:** the first `modprobe cv181x_base` froze the camera. `base` writes
  `vip_sys` (`0x0a0c8000`) at probe; on the Sophgo kernel the VIP clocks never turn off
  (`CLK_IS_CRITICAL` / `CLK_IGNORE_UNUSED`), mainline `clk-cv18xx` turns them off at the end
  of boot (measured: `enable_count` 0). Fix: the `base` node lists those 11 clocks and the
  probe enables them with `devm_clk_bulk_get_all_enabled()` before any access. **Rule for
  every module: no register access before the block's clocks are on.** Later more
  "always on" clocks went to the same node: `csi_be`, `csi0/1_rx`, `csi_mac0/1/2`, `raw`,
  `isp_top`, `cam0_vip`, `sc_top` (the `cif` writes CSI registers without enabling any clock).

### 4.3 Sensor I²C and `snsr_i2c`

- The OV5647 answers at `0x36` on I²C2 with the mainline I²C driver alone: ID
  `0x300A/0x300B = 0x56 0x47`, revision `0x300C = 0x6c`. The Sophgo sensor library starts
  it over the standard `/dev/i2c-2` (`I2C_SLAVE_FORCE`).
- `snsr_i2c.ko` (kernel-side I²C writes synchronised with frames, used by the VI for
  exposure): `i2c_idx` → `adap->nr` (a Sophgo addition to `struct i2c_adapter`; the DT
  `i2cN` aliases keep the numbering); `I2C_M_WRSTOP` → one `i2c_transfer()` per message
  (another Sophgo addition, a STOP after each write: the OV5647's SCCB does not take a
  repeated start); `ERR_PTR` of `i2c_new_client_device()` handled.

### 4.4 `cif` (`cvi_mipi_rx.ko`) and `vi`

- **cif:** the Sophgo 5.10 pinmux headers copied into the package (`kernel-5.10-headers/`);
  sensor reset by `devm_gpiod_get_index_optional` (`snsr-reset-gpios`; the reCamera has
  none). **MCLK** through the clock framework (`COMMON_CLK_CVITEK` defaults on in the
  Sophgo Kconfig, so every factory build uses `clk_set_rate(clk_cam0)`): 23.76 MHz
  (1188/50) instead of 24. MCLK1 is refused (`-EOPNOTSUPP`): it would reprogram `cam1pll`,
  in use on 7.2.
- **vi** (21,000 lines): no `streamline_annotate.h`; `MIN`/`MAX` guards; timer API
  (`timer_container_of`, `timer_delete_sync`); `copy_from_user` return checked; 40 `memcpy`
  of string literals with `sizeof(dest)` → `strscpy` (they read past the literal);
  `sched_setscheduler` (not exported) → `sched_set_fifo` (FIFO 50 instead of 90).
- **Load average ~3 at idle** after loading the VI: the `cvitask_isp_*` threads sleep in
  uninterruptible wait (`D`), counted in the load at 0 % CPU. The four VI thread loops now
  use `wait_event_idle` (`TASK_IDLE`): same behaviour, load average 0.13 at idle.

### 4.5 First frame: the MIPI lane mapping

The whole pipeline came up (`OV5647 1080P 30fps 10bit LINE Init OK!`, ISP thread,
`vi_start_streaming`) **but no frame arrived**: 0 CSI/ISP interrupts, `VISofCh0Cnt = 0`,
data lanes `hs_idle`, D3 always `hs_err`, the clock lane state changing between runs. The
sensor confirmed it was streaming (`0x0100=1`, 2 lanes, 10-bit, 1920×1080).

Discarded on the way: `CAM_MCLK0` pinmux, MCLK frequency (37.125 → 23.76 MHz), the
`MIPIRX0–3` pads in GPIO, pad pull-downs, PHY resets, `-fpack-struct`. Our capture program
**also failed on the factory kernel**, which cleared the kernel port.

**Cause:** on the reCamera the OV5647 is wired with **the clock on physical lane 2 and data
on lanes 0 and 3: `lane_id = {2, 0, 3, -1, -1}`**. The `{0, 1, 2}` of the Sophgo
`ov5647_cmos_param.h` (byte-identical in the factory library) does not fit this board; the
Seeed `sscma-node` overrides it at runtime. Measured on the factory system with the camera
started by `sscma-node` itself (over MQTT: `clear` → `create {type: camera, ...}` →
`enabled true`) and `/proc/mipi-rx`: `LinkId 2, 0, 3`, `D2 hs_sync`, `D3 hs_hst`,
`CK_HS 1`. The "clock lane" we configured (physical 0) was a data lane, hence `CK_HS`
coming and going.

Fixed in the library default (`sophgo-mpi/0002-ov5647-recamera-lane-mapping.patch`). First
frame on Linux 7.2: 1920×1080 NV21, AE active (32.6 ms exposure, ISO 100, LV 6.15).

### 4.6 VPSS

`cv181x_vpss.ko` (20,000 lines): `MIN`/`MAX`, 53 `strncpy` → `strscpy`, `pde_data`,
`UTS_VERSION`, `remove()` → `void`, `sched_set_fifo` (the Sophgo comment itself said "same
as sched_set_fifo"), `chip.mk` options (`CVI_LOG`, `REG_DUMP`, `TILE_MODE`). The VPSS turns
on its own `img_d/v`, `sc_d/v1-3` clocks when opened. Two channels from one frame: ch0
640×640 RGB with `ASPECT_RATIO_AUTO` and a `0x727272` background (the YOLO letterbox), ch1
1280×720 NV21. The VPSS can normalise in hardware (`VPSS_NORMALIZE_S`).

### 4.7 JPEG and H.264 (`cv181x_vcodec.ko`, `cv181x_jpeg.ko`, `cvi_vc_driver.ko`)

- `cvi_vc_driver` (~94,000 lines): the Chips&Media VPU/JPU API and the MPI venc/vdec
  contexts (`/dev/cvi_vc_enc0..N`; each one is a **channel**, the codec is chosen at channel
  creation). Port: `DEFINE_SEMAPHORE(name, 1)` (the count is explicit now); no
  `get_fs`/`set_fs`; `mutex_lock_interruptible` → `mutex_lock` (several callers ignored the
  return and then released a mutex they never took).
- The JPEG IRQ: 7.2 no longer fills `IORESOURCE_IRQ` from the DT, so `jpu irq number irq = 0`
  in `dmesg` is expected; Sophgo never registers it anyway (polling).
- `/etc/modprobe.d/cvi_vc_driver.conf`: `MaxVencChnNum=3 MaxVdecChnNum=0` (Sophgo made 16
  encode and 64 decode channels; the camera only encodes).
- **Firmware:** the H.264 encoder is a Chips&Media CODA980 with its own microcontroller;
  `cvi_vc_driver` loads `/usr/share/fw_vcodec/coda980.bin` on the first H.264 channel
  (`monet.bin` for H.265). Without it `CreateChn` fails cleanly.
- **H.264 profile:** with `u32Profile = H264E_PROFILE_BASELINE` the stream came out **Main**.
  The CODA980 **derives** `profile_idc` from the entropy coder (CABAC → Main, CAVLC →
  Baseline, 8×8 transform → High); the encoder starts with CABAC fixed, and the CAVLC that
  `cviSetChnDefault` computes for Baseline only reaches it through
  `CVI_VENC_SetH264Entropy()`. **Rule:** after `CreateChn` of an H.264 channel always call
  `CVI_VENC_SetH264Entropy()` (Baseline: CAVLC on I and P). Result: **Constrained
  Baseline**, level 3.1, `profile-level-id 42401f`, which every WebRTC client must accept.
- **Measured:** JPEG 1280×720 at 29.9 fps (sensor-bound), ~105 KB per frame; H.264
  1280×720 at 30.0 fps, 2.3 Mbit/s, 150/150 frames decoded by `ffmpeg`.

### 4.8 NPU (`cv181x_tpu.ko` + `cviruntime`)

- Port: `class_create`, `pde_data`, timers, `remove()` → `void`,
  `dma_buf_map_attachment_unlocked` (since 6.2 the plain variant needs `dma_resv` locked),
  cache rule of §6.1. **Clocks:** the Sophgo driver never enables `clk_tpu`/`clk_tpu_fab`;
  on 7.2 they were off (`enable_count` 0); the probe enables them now.
- Package `sophgo-cviruntime`: `flatc` 1.11 for the host → cvibuilder headers →
  `cvikernel` and `cviruntime` for musl. Patch 0001: no git version, `getChipType` without
  an uninitialised pointer in Release, a bounded `snprintf` in the PMU, the vendor `tool/`
  (does not build with GCC 15) off.
- The VPSS ch0 (640×640 `RGB_888` packed) goes to the TPU **without a copy**
  (`CVI_NN_SetTensorPhysicalAddr`). DFL decoding, sigmoid and NMS are ours.
- **Measured** with the factory YOLO11n detection model: `person` in 30 of 30 frames,
  scores 0.82–0.93, **24 fps** (40 ms per frame, inference + decoding) with the sensor at
  30 fps.

## 5. Userspace (`sophgo-mpi`)

- Builds `libsys`, `libvi`, `libvpss`, `libvenc`, `libvo` (the library only; `libcvi_bin`
  needs it), `libisp` with the 3A (`libisp_algo`, `libae`, `libawb`, `libaf`), `libcvi_bin`,
  `libcvi_bin_isp` and **`libsns_ov5647`**, all with `-Werror`. The Seeed tuning is
  installed at `/mnt/cfg/param/cvi_sdr_bin`.
- `TARGET_MACHINE=riscv64-unknown-linux-musl` to use the closed musl objects; our code is
  `rv64gc`.
- **The closed 3A objects** are `rv64imafdc` + `v0p7`, `zvamo0p7`, `zvlsseg0p7`, `xtheadc`:
  ~4,000 T-Head scalar instructions (`custom-0`, enabled by the factory OpenSBI) and ~270
  vector 0.7 instructions. 7.2 has `CONFIG_RISCV_ISA_XTHEADVECTOR=y` (`xtheadvector` in
  `/proc/cpuinfo`); binutils 2.45 links them without complaint.
- Patch `0001-fix-gcc15-warnings.patch`, including **a real bug** in `teaisp_pq_ctrl.c`:
  `leave_scene_snow()` copied `snow_tun_attr[0]` onto itself instead of into `[1]`, as the
  comment says.
- **`recamera-cam`** (`external/package/sophgo-mpi/recamera-cam.c`): a small C library over
  the MPI that brings up sensor/VI/ISP/VPSS/VENC with the measured configuration, reclaims
  what a killed process left (§7), and exposes the ISP picture controls. The Rust
  application links it statically. `recamera-vicap` is the command-line test tool built on
  the same code.

### 5.1 Smaller sensor modes

The Sophgo library only has `OV5647_MODE_1920X1080P30`. Patch
`sophgo-mpi/0003-ov5647-1280x960-640x480-modes.patch` adds, from the register tables of
the mainline `ov5647.c`:
- **1280×960 at 30 fps:** the whole sensor (2592×1944) binned 2×2 → 1296×972, 1280×960
  centred out;
- **640×480 at 30 fps:** the VGA window with binning and skipping (`0x3814`/`0x3815` = `0x35`).

PLL, MIPI timing, HTS 2416 and VTS 1104 stay the Sophgo ones (same pixel clock, line time
and exposure limits as 1080p); `0x3821` without the mirror bit (Sophgo orientation).

**Finding: the sensor output width must be a multiple of 64.** The 1296×972 mode of the
mainline driver does not work on the SG2002 ISP: frames arrive over MIPI without ECC/CRC
errors, pass FE/BE, but post-processing never ends (`VIPostCnt = 0`) and the CSI bridge
overflows (`VICsiOverFlowCnt` = frame count). 1280×960 passes. Diagnosed with
`/proc/cvitek/vi_dbg` (`VISofCh0Cnt`, `VIPreFECh0Cnt`, `VIPostCnt`, `VICsiOverFlowCnt`)
and `/proc/mipi-rx`.

| Mode | Field of view | YOLO input | ION peak | Stream | YOLO |
|---|---|---|---|---|---|
| 1920×1080 | central 16:9 crop | 640×360 | 28.4 MB | 30 fps | 24.6 fps |
| **1280×960** | **~whole sensor (4:3)** | **640×480** | **26.5 MB** | 30 fps (720p, no scaling) | 25.2 fps |
| 640×480 | ~whole sensor (4:3) | 640×480 | 24.3 MB | 30 fps (720p upscaled) | 25.8 fps |

The 4:3 modes show ~35 % more scene horizontally. **The application uses 1280×960**; the
stream channel crops the central 16:9 band (`CVI_VPSS_SetChnCrop`), giving 1280×720
without scaling.

## 6. Findings that apply to the whole port

### 6.1 Cache invalidation on 7.2 RISC-V

- **Symptom:** the first `.h264` did not decode (`sps_id 32 out of range`,
  `non-existing PPS`): the PPS packet held the bytes of the SPS.
- **Mechanism:** the encoder writes the SPS and then the PPS at the **same** address of the
  bitstream buffer; the driver copies each with the CPU; copying the PPS, the CPU read the
  old cache line (the SPS) because no invalidate happened.
- **Cause:** on the Sophgo 5.10, `arch_sync_dma_for_device(…, DMA_FROM_DEVICE)` invalidated.
  On **7.2 RISC-V** the same call **only cleans** (`arch_sync_dma_clean_before_fromdevice()`
  = true); the invalidate moved to `arch_sync_dma_for_cpu()`. The port had replaced the
  unexported function with `dma_sync_single_for_device(DMA_FROM_DEVICE)`: **every
  invalidate was a no-op**.
- **Fix:** invalidate = `dma_sync_single_for_cpu(dev, pa, len, DMA_FROM_DEVICE)` in
  `sys_cache_invalidate`, the cache ioctl of `/dev/cvi-sys` and
  `ION_IOC_CVITEK_INVALIDATE_PHY_RANGE`; flush stays `dma_sync_single_for_device(DMA_TO_DEVICE)`.
- **Reach:** anything written by hardware and read by the CPU after an invalidate: VI/VPSS
  frames, codec bitstreams and above all **NPU outputs**, where the error hangs nothing and
  only gives wrong detections now and then. Earlier tests passed because big buffers were
  rarely in the cache: the failure was intermittent.

### 6.2 `fence.tso` and the T-Head C906

- **Symptom:** a process died with SIGILL (exit 132) at exit, in
  `std::_Sp_counted_base::_M_release_last_use` (`libcviruntime.so`).
- **Cause:** GCC 15 emits `fence.tso` for `__atomic_thread_fence(__ATOMIC_ACQ_REL)`. The
  C906 **does not implement `fence.tso`** and raises an illegal instruction, instead of
  executing it as `fence rw,rw` — which is why the spec encoded `fence.tso` as a `fence`
  with `fm=1000` in the first place.
- **Reach, measured in the image:** `libstdc++.so` 60 occurrences, `gdbserver` 9, `libgomp`
  1, `libcviruntime` 1; the kernel, musl and the C programs none.
- **Semantics:** `fence.tso` orders R→R, R→W and W→W; `fence rw,rw` orders all four pairs.
  Replacing one with the other is a **stronger** barrier, never weaker: always correct.
- **Fix, two layers:**
  1. **Compiler:** GCC patched to not emit `fence.tso` by default
     (`container/patches/gcc/15.3.0/0001-…`, `riscv.opt`: `mfence-tso` → `Init(0)`);
     `libstdc++` and `libgomp` rebuilt with zero occurrences. The application build counts
     them in every binary (must be 0).
  2. **Firmware:** the Armbian OpenSBI patch
     (`patch/opensbi/sophgo-sg200x/0001-lib-sbi_illegal_insn-add-emulation-for-fence-tso.patch`):
     in `sbi_illegal_insn.c`, a `fence.tso` runs `smp_mb()`, steps the PC by 4 and returns.
     It covers any binary (Rust, third party) at one trip to M-mode per occurrence
     (**0.32 µs** measured). Upstream OpenSBI 1.7 carries the same emulation; the vendor
     0.9 in the board's fip does not, so the fip was rebuilt with only its OpenSBI replaced
     (`external/package/recamera-fip`, checked against the original byte for byte).
- **Test:** `tests/f7/fencetso.c` runs a hand-encoded `fence.tso` (`.word 0x8330000f`):
  SIGILL on the factory fip, exit 0 on ours (100,000 runs).

### 6.3 An encoder channel without `u32BufSize` hangs the SoC

Without a size the JPEG channel asks for `LARGE_STREAM_BUF_SIZE` = **9 MiB** (sized for
4096×2304). With VI + VPSS the 32 MiB ION was 79 % full; the allocation failed
(`ion allocated len=0x900000 failed`) and **the SoC froze**. Always set `u32BufSize`
(a multiple of 1024; 1 MiB for 720p). The Sophgo error path after that failure is not safe.

## 7. A killed process must not leak or hang the camera

The MPI objects live in the kernel drivers, which do not free them when the owning process
dies (`kill -9`, crash): ~20 MB of ION stayed taken until reboot, and the next run failed
with `VPSS_CreateGrp 0xc0068004` (group exists).

What each object needed:
- **VPSS groups:** a new process destroys them by number.
- **Encoder channels:** live in `cvi_vc_driver`, one `/dev/cvi_vc_enc<N>` per channel; the
  cleanup talks to the devices directly (listed in `/proc/cvitek/venc`).
- **VB pools:** a reference count in the kernel per `CVI_VB_Init`, cleared at 0;
  `CVI_VB_Exit` only reaches the kernel from the process that did `CVI_VB_Init`, so the dead
  one's reference goes through libsys's exit ioctl (`vb_ioctl_exit`).
- **Userspace ION buffers** (`VI_DMA_BUF`, `ISP_SHARED_BUFFER_0`, allocated through
  `cvi-sys`): the driver keeps a reference only `SYS_ION_FREE` drops. **osdrv patch 0002:**
  each allocation records its owning `/dev/cvi-sys` file; when that file closes the buffers
  become orphans, freed by a new ioctl `SYS_ION_FREE_ORPHANS`. They **cannot** be freed at
  close: a dying process closes `/dev/cvi-sys` before `/dev/cvi-vi`, and the VI keeps
  writing into `VI_DMA_BUF` until its own release stops the hardware — the first version
  freed them there and **hung the SoC**.
- **`vi_cmdq_buf`** (128 KiB, allocated by the VI driver): only `DISABLE_CHN` freed it.
  **osdrv patch 0003** frees it at release.
- Two Sophgo bugs fixed in 0002: `SYS_ION_FREE` called `close_fd()` in the **current**
  process (from another process it would close some random fd of the caller); and the
  owner was recorded per thread (`pid`) instead of per process (`tgid`).

`rc_reclaim()` in `recamera-cam` acts only if **no** live process has `/dev/cvi-base`,
`/dev/cvi-vi` or `/dev/cvi-vpss` open, so it never tears down a running pipeline.

**A hang on `kill -9`, found with pstore:** 1 in 10–20 kills froze the SoC (watchdog reset).
The kernel log kept by ramoops showed the online pipeline stopped at the moment of the kill,
and no soft-lockup report: a hard hang, the CPU stuck on a hardware access. **Cause:** the
kernel closes a dead process's files in fd order, and libsys opens `/dev/cvi-vi` before
`/dev/cvi-vpss`, so the VI release stopped the ISP while the online VPSS group was still
reading from it — the reverse of the normal teardown. **Fix without a kernel patch:**
`recamera-cam` duplicates the VI fd above 900 (`F_DUPFD_CLOEXEC`), so the VI's last
reference closes after the VPSS's. Verified: 60 kills in a row without a hang, ION back to
0 every time.

## 8. Memory: sizing ION

`scripts/ion-measure.sh '<command>'` samples the ION heap every 200 ms while a command runs
and prints the peak, the allocations at the peak and the heap afterwards.

**Online VI→VPSS** (`VI_OFFLINE_VPSS_ONLINE`, VPSS in `VPSS_MODE_DUAL`, device 1 with
`VPSS_INPUT_ISP`, the group on that device, no bind — the recipe of the SDK's
`sample_venc_lib.c`): the ISP output goes straight to the scaler and **the 1080p VI frame
pool (9.1 MB) disappears**.

| Part | ION |
|---|---|
| VI + ISP (`VI_DMA_BUF` 5.4 MB, ISP 0.3, cmdq 0.1) | 5.8 MB |
| VPSS ch0 640×640 RGB (3 blocks) | 3.6 MB |
| VPSS ch1 720p NV21 (3 blocks) | 4.1 MB |
| H.264 (2 reconstruction frames, firmware, bitstream, work) | 4.7 MB |
| JPEG (bitstream) | 1.0 MB |
| YOLO11n detection on the NPU | 6.6 MB |

| Configuration | ION peak | Stream (H.264 + JPEG) | YOLO |
|---|---|---|---|
| offline, 3 blocks | ≈ 35 MB: **does not fit** 32 MiB | — | — |
| online, 1 block | 19.1 MB | **stalls** | — |
| online, 2 blocks | 23.3 MB | 14.8 fps | 14.9 fps |
| online, 3 blocks | 25.9 MB | 23.1 fps | 19.5 fps |
| **online, 4 blocks** | **28.4 MB** | **29.9 fps** | **24.1 fps** |

The bottleneck was the number of VPSS blocks, not the CPU (50 % idle under load). NPU work
areas: 2.6 MB (detection), 2.9 MB (pose), 6.4 MB (segment). ION was first sized at 36 MiB
for detection (stream at 30.0 fps with YOLO at 24.3 fps, peak 28.4 MB).

**ION is 48 MiB** (`size = <0x03000000>`) since the application runs any model type: the
segment model's float outputs add ~7 MB (`[1,116,8400]` + `[1,32,160,160]` in f32), and
with it the pipeline alone took 34.6 of 36 MB. Measured peaks with RTSP and MJPEG open:
detection 25.8 MB, pose 27.3 MB, segment 36.0 MB. Linux keeps ~164 MB available.

**When ION runs out under the H.264 encoder, the codec hangs** (21/10/2026): the encoder
allocates its reconstruction frames at the first frame (`fail to allocate recon buffer`,
`CVI_VENC_SendFrame ... -11`), and after that failure its threads stay in the driver
(`cvi_vc_drv_venc_ioctl`, the next user in `EnterVcodecLock`): the pipeline cannot be torn
down and the process becomes a zombie until a reboot. The daemon now checks the free ION
after loading the model and opening the encoders, and unloads a model that leaves less than
~5.5 MB.

## 9. Added along the way

- **White LED with brightness** (patch 0022): the LED is on pad `PWR_GPIO0`, function 4 =
  `PWM_8`. A new `pwm-cv1800` driver for the 7.2 PWM API (register logic from Sophgo's
  `cvi_pwm.c`; 0 % and 100 % with the channel stopped and the level set by polarity;
  `CLK_PWM` enabled at probe), `leds-pwm`, **25 kHz** (far above the sensor line rate: no
  banding). Measured in a dark room: at 255 the scene gains +0.58 EV and the ISP lowers the
  gain; the benefit is **less noise**, not a brighter picture.
- **Watchdog** (patch 0023): the camera had none (`/dev/watchdog` missing). DesignWare WDT
  at `0x3010000`; the `sophgo,cv1800-wdt` compatible sets bit 8 of `TOP + 0x8` at probe, as
  Sophgo's `cvi_wdt.c` (that register is write-only: it always reads 0). `S02watchdog` runs
  BusyBox `watchdog` (30 s timeout, fed every 10 s). Side effect fixed: `dw_wdt` registers a
  restart handler at priority 128, above our RTC reboot, so every `reboot` went through the
  watchdog; patch 0009 now uses `SYS_OFF_PRIO_HIGH`.
- **Power-on reason** (`RTC_ST_ON_REASON` = `0x050260F8`), calibrated:

  | Value | Cause |
  |---|---|
  | `0x000D0000` | power-on after a power cut |
  | `0x04090003` | software reboot (warm reset) |
  | `0x08090003` | watchdog reset |

- **Crash log across the reset:** `CONFIG_PSTORE_RAM` + `ramoops@8ff00000` (1 MiB: 512 KiB
  console, 128 KiB pmsg, 3 oops/panic records) and a 20 s soft-lockup detector (before the
  30 s watchdog). The watchdog reset is warm and the DRAM keeps its contents: a marker
  written to `/dev/pmsg0` came back after the reset with the previous boot's console.
- **Thermal driver** (patch 0021): see `docs/hardware.md` §7b. It does **not** enable the
  hardware overheat reset (`overheat_reset_en`), as neither the FSBL nor U-Boot do.
- **Out-of-tree modules built for another kernel:** enabling `CONFIG_DETECT_HUNG_TASK` added
  `last_holder` to `struct semaphore`; Buildroot did not rebuild `sophgo-osdrv`, the old
  `.ko` files still loaded, and the new kernel's `down()`/`up()` wrote past the modules'
  semaphores (an Oops in `vpu_open_instance`, encoder semaphores stuck without an owner).
  Rebuild the three `kernel-module` packages with the kernel; `CONFIG_MODVERSIONS=y` now
  refuses a mismatched module at load.

# reCamera-OS 0.2.4 — vendor tree pins

Fresh shallow clone (`--depth 1`) of the tag, into `vendor/reCamera-OS/` of this project.

    git clone --depth 1 --branch 0.2.4 https://github.com/Seeed-Studio/reCamera-OS.git reCamera-OS
    git -C reCamera-OS submodule update --init --recursive --depth 1 --jobs 4

- HEAD: `01debc9967752448e29a745f37d29d80a4a41e37` (`grafted, HEAD, tag: 0.2.4`)
- `git status --porcelain` = 0 (clean); 27 pins = 25 top-level submodules + 2 nested in FreeRTOS-Kernel
- Size measured on 2026-10-16: 16 GB

## Pins (`git submodule status --recursive`)

    d52c1b6e6193e0ab08950120068a3f8fe6beab1c  FreeRTOS-Kernel
    f0618d9e2f4c5b0a3e472a2673a090e8ef836258  FreeRTOS-Kernel/portable/ThirdParty/Community-Supported-Ports
    3f9c99a682c5c796bb7eb89fd9c4385688fce27a  FreeRTOS-Kernel/portable/ThirdParty/Partner-Supported-Ports
    5042bfd07b1a176d25e24424c4751a497e3bf83a  Lab-Project-FreeRTOS-POSIX
    86b0708e0101dfd4b60abafe98fef28c92773dbb  SensorSupportList
    85f705b17cfce87c82020786ba8a7fce5299e4ac  build
    5b7acd1bf99574b818f9ec1f61232c69f6150202  buildroot-2021.05
    2f56f4c23ea840f9d15c43801368cf4a089efa84  cnpy
    6dbbbb8bed877cd3a08f76ce4aed61645c38bbe1  cvi_mpi
    a6ba76b33a09df30810023105533002b92b6d768  cvi_rtsp
    4309f2a649fc7cfe7160389d52a81c469dbdd7bc  cvibuilder
    9f1f57a19c3c281a931dfc71b318494487193d56  cvikernel
    ce8705f49da5e5f59c2ddb3253ef88323a0cd9c4  cvimath
    3f4938648950a7f3bf9a19c320ca9fae7c52de20  cviruntime
    6da1cf79d90eb242e7da5318241d42279a3df3ba  flatbuffers
    eb9faf5fe6e63968bc47cb15eca87616445db8f3  freertos
    92735b967c951883c478bae07ca171fa758426c6  fsbl
    b4c069a46a99433d79de9308f7c87591582e39fc  host-tools
    f7b23013ad67d902fa8ad03eb6d6f68c96717d0b  isp_tuning
    d39861011a8da11c68470af4155d7e121c8df32e  ive
    d0486a96f7e8756c663acbc6a57371360f38871f  linux_5.10
    6766e9cc96df51cde91540f9a15f291bf843b682  opensbi
    f35ca075cc022aac97f5d22238c1e46caf8276bd  osdrv
    edd2b4fc07f9ee5f5b4a04430b9a67e4f714db01  oss
    f855ea377c886aae094e224f4dffebcffaedc4ad  ramdisk
    1d86f4dd7ef97a01fa931bfa3d6eaf7b5303a4bf  tdl_sdk
    050d49a7b19289407c9f9998e16869c97ced891c  u-boot-2021.10

All submodules come from `github.com/sophgo/*`. The Buildroot one is named
`buildroot-2021.05` (branch `sg200x-dev`) — it is where the vendor side keeps the
Buildroot reference defconfig used by phase F2.

## sg2002 references inside the vendor tree

    reCamera-OS/external/configs/sg2002_recamera_emmc_defconfig
    reCamera-OS/external/configs/sg2002_xiao_sd_defconfig
    reCamera-OS/build/boards/cv181x/sg2002_wevb_arm64_sd
    reCamera-OS/build/boards/cv181x/sg2002_wevb_riscv64_sd

## Other revisions of the tag (`git ls-remote --tags --refs`, 2026-10-16)

    0.2.0  d5be2c644f7733718b9617fb13079efbffb90706
    0.2.1  a68bbcfdb86d306ffb02ad0a7015c203f9459c63
    0.2.2  12c9d69b39147eb2988bca4f074743972c9e935d
    0.2.3  53075423349729311eeac644ac3d4fe247a38e7a
    0.2.4  01debc9967752448e29a745f37d29d80a4a41e37

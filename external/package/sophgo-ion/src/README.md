# Sophgo/Cvitek ION sources

The files of the Android staging ION allocator and its Cvitek glue that the
Sophgo multimedia drivers need, copied unmodified from the vendor kernel:

- repository: https://github.com/sophgo/linux_5.10 (the `linux_5.10`
  submodule of Seeed-Studio/reCamera-OS 0.2.4)
- commit: d0486a96f7e8756c663acbc6a57371360f38871f
- path: drivers/staging/android/ (ion/, ion/cvitek/, uapi/)
- license: GPL-2.0 (Linux kernel)

Only these 10 files are built (see ../Kbuild); keeping them here avoids a
1 GB download of the whole 5.10 kernel for 200 KB of sources. The changes for
Linux 7.2 are in ../0001-port-to-linux-7.2.patch, applied at build time.

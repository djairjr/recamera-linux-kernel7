// Link with recamera-cam (static, from the sophgo-mpi package) and the vendor
// MPI libraries it needs (the list of SOPHGO_MPI_CAM_LIBS in
// external/package/sophgo-mpi/sophgo-mpi.mk), plus the NPU runtime
// (sophgo-cviruntime). The target linker finds the
// shared ones in the Buildroot sysroot; rustc looks for the static one
// itself, in RECAMERA_SYSROOT/usr/lib (set by the recamera-app package). On
// any other target (cargo check on the host) nothing is linked.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=RECAMERA_SYSROOT");
    if std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() != Ok("riscv64") {
        return;
    }
    match std::env::var("RECAMERA_SYSROOT") {
        Ok(s) => println!("cargo:rustc-link-search=native={s}/usr/lib"),
        Err(_) => println!("cargo:warning=RECAMERA_SYSROOT not set: librecamera-cam.a may not be found"),
    }
    println!("cargo:rustc-link-lib=static=recamera-cam");
    for l in [
        "sys", "vi", "vpss", "vo", "venc", "isp", "isp_algo", "ae", "awb", "af", "cvi_bin",
        "cvi_bin_isp", "sns_ov5647", "cviruntime", "cvikernel", "pthread", "dl", "m", "atomic",
    ] {
        println!("cargo:rustc-link-lib=dylib={l}");
    }
}

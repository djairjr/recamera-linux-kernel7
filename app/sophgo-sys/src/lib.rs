//! Hand-written FFI to the Sophgo cv181x MPI (only what the daemon calls) and
//! to recamera-cam (external/package/sophgo-mpi/recamera-cam.h: SYS/VB, the
//! OV5647 bring-up and the cleanup of a killed run).
//!
//! The structs mirror the vendor headers (include/linux/cvi_comm_video.h,
//! cvi_comm_vpss.h). Their sizes and offsets are checked here at compile time
//! against the same numbers recamera-cam.c checks with _Static_assert, so a
//! header change breaks the build on both sides. Reference for every call:
//! external/package/sophgo-mpi/recamera-vicap.c.

#![no_std]
#![allow(non_camel_case_types, non_snake_case)]

use core::ffi::{c_int, c_uint, c_void};
use core::mem::{offset_of, size_of};

/// CVI_S32: 0 or an MPI error code (0xC00x_xxxx)
pub type CviS32 = i32;

// PIXEL_FORMAT_E
pub const PIXEL_FORMAT_RGB_888: i32 = 0;
pub const PIXEL_FORMAT_RGB_888_PLANAR: i32 = 2;
pub const PIXEL_FORMAT_NV12: i32 = 18;
pub const PIXEL_FORMAT_NV21: i32 = 19;
// VIDEO_FORMAT_E
pub const VIDEO_FORMAT_LINEAR: i32 = 0;
// ASPECT_RATIO_E
pub const ASPECT_RATIO_NONE: i32 = 0;
pub const ASPECT_RATIO_AUTO: i32 = 1;
// VPSS_CROP_COORDINATE_E
pub const VPSS_CROP_ABS_COOR: i32 = 1;

pub const VPSS_MAX_CHN_NUM: i32 = 4;

// enum rc_codec
pub const RC_CODEC_JPEG: c_int = 0;
pub const RC_CODEC_H264: c_int = 1;

// enum rc_sensor
pub const RC_SENSOR_1280X960: c_int = 0;
pub const RC_SENSOR_1920X1080: c_int = 1;
pub const RC_SENSOR_640X480: c_int = 2;

#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct RECT_S {
    pub s32X: i32,
    pub s32Y: i32,
    pub u32Width: u32,
    pub u32Height: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct FRAME_RATE_CTRL_S {
    pub s32SrcFrameRate: i32,
    pub s32DstFrameRate: i32,
}

impl Default for FRAME_RATE_CTRL_S {
    /// -1/-1: no frame rate control
    fn default() -> Self {
        FRAME_RATE_CTRL_S { s32SrcFrameRate: -1, s32DstFrameRate: -1 }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct ASPECT_RATIO_S {
    pub enMode: i32,
    pub bEnableBgColor: u8,
    pub u32BgColor: u32,
    pub stVideoRect: RECT_S,
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct VPSS_NORMALIZE_S {
    pub bEnable: u8,
    pub factor: [f32; 3],
    pub mean: [f32; 3],
    pub rounding: i32,
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct VPSS_GRP_ATTR_S {
    pub u32MaxW: u32,
    pub u32MaxH: u32,
    pub enPixelFormat: i32,
    pub stFrameRate: FRAME_RATE_CTRL_S,
    /// VPSS device: 1 for the online (ISP-fed) group of VPSS_MODE_DUAL
    pub u8VpssDev: u8,
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct VPSS_CHN_ATTR_S {
    pub u32Width: u32,
    pub u32Height: u32,
    pub enVideoFormat: i32,
    pub enPixelFormat: i32,
    pub stFrameRate: FRAME_RATE_CTRL_S,
    pub bMirror: u8,
    pub bFlip: u8,
    pub u32Depth: u32,
    pub stAspectRatio: ASPECT_RATIO_S,
    pub stNormalize: VPSS_NORMALIZE_S,
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct VPSS_CROP_INFO_S {
    pub bEnable: u8,
    pub enCropCoordinate: i32,
    pub stCropRect: RECT_S,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct VIDEO_FRAME_S {
    pub u32Width: u32,
    pub u32Height: u32,
    pub enPixelFormat: i32,
    pub enBayerFormat: i32,
    pub enVideoFormat: i32,
    pub enCompressMode: i32,
    pub enDynamicRange: i32,
    pub enColorGamut: i32,
    pub u32Stride: [u32; 3],
    pub u64PhyAddr: [u64; 3],
    pub pu8VirAddr: [*mut u8; 3],
    pub u32Length: [u32; 3],
    pub s16OffsetTop: i16,
    pub s16OffsetBottom: i16,
    pub s16OffsetLeft: i16,
    pub s16OffsetRight: i16,
    pub u32TimeRef: u32,
    pub u64PTS: u64,
    pub pPrivateData: *mut c_void,
    pub u32FrameFlag: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct VIDEO_FRAME_INFO_S {
    pub stVFrame: VIDEO_FRAME_S,
    pub u32PoolId: u32,
}

impl VIDEO_FRAME_INFO_S {
    /// An empty frame for the MPI to fill.
    pub const fn zeroed() -> Self {
        // SAFETY: plain integers and raw pointers, all valid as zero
        unsafe { core::mem::zeroed() }
    }
}

/// struct rc_pool
#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct rc_pool {
    pub blk_size: c_uint,
    pub blk_cnt: c_uint,
}

// The numbers recamera-cam.c asserts for the same structs (riscv64 LP64).
const _: () = {
    assert!(size_of::<VIDEO_FRAME_S>() == 144);
    assert!(size_of::<VIDEO_FRAME_INFO_S>() == 152);
    assert!(offset_of!(VIDEO_FRAME_S, u32Stride) == 32);
    assert!(offset_of!(VIDEO_FRAME_S, u64PhyAddr) == 48);
    assert!(offset_of!(VIDEO_FRAME_S, pu8VirAddr) == 72);
    assert!(offset_of!(VIDEO_FRAME_S, u32Length) == 96);
    assert!(offset_of!(VIDEO_FRAME_S, s16OffsetTop) == 108);
    assert!(offset_of!(VIDEO_FRAME_S, u32TimeRef) == 116);
    assert!(offset_of!(VIDEO_FRAME_S, u64PTS) == 120);
    assert!(offset_of!(VIDEO_FRAME_S, pPrivateData) == 128);
    assert!(offset_of!(VIDEO_FRAME_S, u32FrameFlag) == 136);
    assert!(offset_of!(VIDEO_FRAME_INFO_S, u32PoolId) == 144);
    assert!(size_of::<VPSS_GRP_ATTR_S>() == 24);
    assert!(offset_of!(VPSS_GRP_ATTR_S, stFrameRate) == 12);
    assert!(offset_of!(VPSS_GRP_ATTR_S, u8VpssDev) == 20);
    assert!(size_of::<VPSS_CHN_ATTR_S>() == 92);
    assert!(offset_of!(VPSS_CHN_ATTR_S, stFrameRate) == 16);
    assert!(offset_of!(VPSS_CHN_ATTR_S, bMirror) == 24);
    assert!(offset_of!(VPSS_CHN_ATTR_S, bFlip) == 25);
    assert!(offset_of!(VPSS_CHN_ATTR_S, u32Depth) == 28);
    assert!(offset_of!(VPSS_CHN_ATTR_S, stAspectRatio) == 32);
    assert!(offset_of!(VPSS_CHN_ATTR_S, stNormalize) == 60);
    assert!(size_of::<ASPECT_RATIO_S>() == 28);
    assert!(offset_of!(ASPECT_RATIO_S, bEnableBgColor) == 4);
    assert!(offset_of!(ASPECT_RATIO_S, u32BgColor) == 8);
    assert!(offset_of!(ASPECT_RATIO_S, stVideoRect) == 12);
    assert!(size_of::<VPSS_NORMALIZE_S>() == 32);
    assert!(offset_of!(VPSS_NORMALIZE_S, factor) == 4);
    assert!(offset_of!(VPSS_NORMALIZE_S, rounding) == 28);
    assert!(size_of::<VPSS_CROP_INFO_S>() == 24);
    assert!(offset_of!(VPSS_CROP_INFO_S, enCropCoordinate) == 4);
    assert!(offset_of!(VPSS_CROP_INFO_S, stCropRect) == 8);
};

// cviruntime.h (sophgo-cviruntime): the tensors are used through the
// accessor functions only, so CVI_TENSOR is opaque here, with the size and
// alignment measured with the cross compiler (arrays of it are indexed).

/// CVI_TENSOR (168 bytes, 8-aligned): fields read through CVI_NN_Tensor*()
#[repr(C, align(8))]
pub struct CVI_TENSOR {
    _opaque: [u8; 168],
}

pub const CVI_DIM_MAX: usize = 6;

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct CVI_SHAPE {
    pub dim: [i32; CVI_DIM_MAX],
    pub dim_size: usize,
}

pub type CVI_MODEL_HANDLE = *mut c_void;

const _: () = {
    assert!(size_of::<CVI_TENSOR>() == 168);
    assert!(size_of::<CVI_SHAPE>() == 32);
    assert!(offset_of!(CVI_SHAPE, dim_size) == 24);
};

unsafe extern "C" {
    // cviruntime.h
    pub fn CVI_NN_RegisterModel(model_file: *const core::ffi::c_char, model: *mut CVI_MODEL_HANDLE) -> c_int;
    pub fn CVI_NN_GetInputOutputTensors(
        model: CVI_MODEL_HANDLE,
        inputs: *mut *mut CVI_TENSOR,
        input_num: *mut i32,
        outputs: *mut *mut CVI_TENSOR,
        output_num: *mut i32,
    ) -> c_int;
    pub fn CVI_NN_Forward(
        model: CVI_MODEL_HANDLE,
        inputs: *mut CVI_TENSOR,
        input_num: i32,
        outputs: *mut CVI_TENSOR,
        output_num: i32,
    ) -> c_int;
    pub fn CVI_NN_CleanupModel(model: CVI_MODEL_HANDLE) -> c_int;
    pub fn CVI_NN_TensorName(tensor: *mut CVI_TENSOR) -> *mut core::ffi::c_char;
    pub fn CVI_NN_TensorPtr(tensor: *mut CVI_TENSOR) -> *mut c_void;
    pub fn CVI_NN_TensorSize(tensor: *mut CVI_TENSOR) -> usize;
    pub fn CVI_NN_TensorQuantScale(tensor: *mut CVI_TENSOR) -> f32;
    pub fn CVI_NN_TensorShape(tensor: *mut CVI_TENSOR) -> CVI_SHAPE;
    pub fn CVI_NN_SetTensorPhysicalAddr(tensor: *mut CVI_TENSOR, paddr: u64) -> c_int;
}

unsafe extern "C" {
    // recamera-cam.h
    pub fn rc_cam_size(sensor: c_int, width: *mut c_uint, height: *mut c_uint, vi_blk_size: *mut c_uint) -> c_int;
    pub fn rc_buffer_size(width: c_uint, height: c_uint, pixfmt: c_int) -> c_uint;
    pub fn rc_reclaim() -> c_int;
    pub fn rc_sys_open(pools: *const rc_pool, npools: c_uint, online: c_int) -> c_int;
    pub fn rc_sys_close();
    pub fn rc_cam_open(sensor: c_int) -> c_int;
    pub fn rc_cam_close();
    pub fn rc_isp_image(brightness: c_uint, contrast: c_uint, saturation: c_uint, hue: c_uint, sharpness: c_uint)
        -> c_int;
    pub fn rc_cam_flip(mirror: c_int, flip: c_int) -> c_int;
    pub fn rc_venc_open(chn: c_int, codec: c_int, width: c_uint, height: c_uint, param: c_uint, gop: c_uint)
        -> c_int;
    pub fn rc_venc_close(chn: c_int);
    pub fn rc_venc_request_idr(chn: c_int) -> c_int;
    pub fn rc_venc_encode(
        chn: c_int,
        frame: *const VIDEO_FRAME_INFO_S,
        timeout_ms: c_int,
        buf: *mut u8,
        cap: c_uint,
        len: *mut c_uint,
    ) -> c_int;

    // cvi_vpss.h
    pub fn CVI_VPSS_CreateGrp(grp: i32, attr: *const VPSS_GRP_ATTR_S) -> CviS32;
    pub fn CVI_VPSS_DestroyGrp(grp: i32) -> CviS32;
    pub fn CVI_VPSS_StartGrp(grp: i32) -> CviS32;
    pub fn CVI_VPSS_StopGrp(grp: i32) -> CviS32;
    pub fn CVI_VPSS_ResetGrp(grp: i32) -> CviS32;
    pub fn CVI_VPSS_SetChnAttr(grp: i32, chn: i32, attr: *const VPSS_CHN_ATTR_S) -> CviS32;
    pub fn CVI_VPSS_SetChnCrop(grp: i32, chn: i32, crop: *const VPSS_CROP_INFO_S) -> CviS32;
    pub fn CVI_VPSS_EnableChn(grp: i32, chn: i32) -> CviS32;
    pub fn CVI_VPSS_DisableChn(grp: i32, chn: i32) -> CviS32;
    pub fn CVI_VPSS_GetChnFrame(grp: i32, chn: i32, frame: *mut VIDEO_FRAME_INFO_S, timeout_ms: i32) -> CviS32;
    pub fn CVI_VPSS_ReleaseChnFrame(grp: i32, chn: i32, frame: *const VIDEO_FRAME_INFO_S) -> CviS32;

    // cvi_sys.h
    pub fn CVI_SYS_Mmap(phys: u64, size: u32) -> *mut c_void;
    pub fn CVI_SYS_MmapCache(phys: u64, size: u32) -> *mut c_void;
    pub fn CVI_SYS_Munmap(virt: *mut c_void, size: u32) -> CviS32;
    pub fn CVI_SYS_IonInvalidateCache(phys: u64, virt: *mut c_void, len: u32) -> CviS32;
}

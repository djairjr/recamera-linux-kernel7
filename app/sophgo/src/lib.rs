//! Safe layer over the Sophgo MPI for the reCamera (docs/application.md
//! §3). Each MPI object is an owner that tears it down in `Drop`, and the
//! borrows force the teardown order the MPI needs:
//!
//! ```text
//! Sys (SYS/VB)  >  Camera (sensor, VI, ISP)  >  VpssGroup  >  Frame
//! ```
//!
//! A process killed without its teardown leaves these objects in the kernel:
//! call [`reclaim`] before [`Sys::open`].

pub mod nn;
pub mod venc;
pub mod yolo;

use sophgo_sys as ffi;
use std::fmt;
use std::marker::PhantomData;

/// An MPI call that failed, with its error code (0xC00x_xxxx).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Error {
    pub op: &'static str,
    pub code: i32,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "{}: 0x{:08x}", self.op, self.code as u32)
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

fn check(op: &'static str, code: i32) -> Result<()> {
    if code == 0 { Ok(()) } else { Err(Error { op, code }) }
}

/// The OV5647 modes (sophgo-mpi patch 0003).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sensor {
    /// 2x2 binned, ~full field of view: the application's mode
    R1280x960,
    /// centre crop
    R1920x1080,
    /// binned + skipped
    R640x480,
}

impl Sensor {
    pub fn from_name(s: &str) -> Option<Self> {
        match s {
            "1280x960" => Some(Sensor::R1280x960),
            "1920x1080" => Some(Sensor::R1920x1080),
            "640x480" => Some(Sensor::R640x480),
            _ => None,
        }
    }

    fn raw(self) -> i32 {
        match self {
            Sensor::R1280x960 => ffi::RC_SENSOR_1280X960,
            Sensor::R1920x1080 => ffi::RC_SENSOR_1920X1080,
            Sensor::R640x480 => ffi::RC_SENSOR_640X480,
        }
    }

    /// Output size of the mode.
    pub fn size(self) -> Result<(u32, u32)> {
        let (mut w, mut h) = (0, 0);
        // SAFETY: plain out-parameters; vi_blk_size may be NULL
        check("rc_cam_size", unsafe { ffi::rc_cam_size(self.raw(), &mut w, &mut h, std::ptr::null_mut()) })?;
        Ok((w, h))
    }
}

/// Pixel formats used by the daemon (PIXEL_FORMAT_E).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelFormat {
    /// RGB packed: the YOLO cvimodel input
    Rgb888,
    Rgb888Planar,
    /// Y + interleaved VU: the encoders' input
    Nv21,
}

impl PixelFormat {
    fn raw(self) -> i32 {
        match self {
            PixelFormat::Rgb888 => ffi::PIXEL_FORMAT_RGB_888,
            PixelFormat::Rgb888Planar => ffi::PIXEL_FORMAT_RGB_888_PLANAR,
            PixelFormat::Nv21 => ffi::PIXEL_FORMAT_NV21,
        }
    }
}

/// Bytes of one frame: the block size of its VB pool.
pub fn buffer_size(width: u32, height: u32, fmt: PixelFormat) -> u32 {
    // SAFETY: pure computation
    unsafe { ffi::rc_buffer_size(width, height, fmt.raw()) }
}

/// What [`reclaim`] found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reclaim {
    /// objects of a killed run destroyed (0: nothing was left)
    Cleaned(u32),
    /// a live process (pid) uses the MPI: nothing was touched
    Busy(u32),
}

/// Destroy what a killed run left in the kernel (encoder channels, VPSS
/// groups, VI pipe, VB pools). Safe when nothing is left.
pub fn reclaim() -> Reclaim {
    // SAFETY: no arguments; checks for other users itself
    let r = unsafe { ffi::rc_reclaim() };
    if r < 0 { Reclaim::Busy(r.unsigned_abs()) } else { Reclaim::Cleaned(r as u32) }
}

/// One VB pool: `count` blocks of `size` bytes.
#[derive(Debug, Clone, Copy)]
pub struct Pool {
    pub size: u32,
    pub count: u32,
}

/// SYS + VB. One per process.
pub struct Sys {
    online: bool,
    _not_send: PhantomData<*const ()>,
}

impl Sys {
    /// Pools smallest block first (the VB takes the first that fits).
    /// `online`: VI -> VPSS online (no VI frame pool; VPSS groups on device 1).
    pub fn open(pools: &[Pool], online: bool) -> Result<Sys> {
        let p: Vec<ffi::rc_pool> =
            pools.iter().map(|p| ffi::rc_pool { blk_size: p.size, blk_cnt: p.count }).collect();
        // SAFETY: p lives across the call; rc_sys_open copies it
        check("rc_sys_open", unsafe { ffi::rc_sys_open(p.as_ptr(), p.len() as u32, online as i32) })?;
        Ok(Sys { online, _not_send: PhantomData })
    }

    pub fn online(&self) -> bool {
        self.online
    }
}

impl Drop for Sys {
    fn drop(&mut self) {
        // SAFETY: opened by Sys::open; everything borrowing it is gone
        unsafe { ffi::rc_sys_close() };
    }
}

/// The sensor, MIPI, VI and ISP (with its 3A thread), running.
pub struct Camera<'s> {
    sys: &'s Sys,
    width: u32,
    height: u32,
}

impl<'s> Camera<'s> {
    pub fn open(sys: &'s Sys, sensor: Sensor) -> Result<Camera<'s>> {
        let (width, height) = sensor.size()?;
        // SAFETY: SYS is open (borrowed)
        check("rc_cam_open", unsafe { ffi::rc_cam_open(sensor.raw()) })?;
        Ok(Camera { sys, width, height })
    }

    pub fn size(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Picture adjustments, each 0..100 (50: as tuned).
    pub fn set_image(&self, i: &Image) -> Result<()> {
        // SAFETY: the camera is open (self)
        check("rc_isp_image", unsafe {
            ffi::rc_isp_image(i.brightness, i.contrast, i.saturation, i.hue, i.sharpness)
        })
    }

    /// Mirror (left-right) and flip (upside down).
    pub fn set_flip(&self, mirror: bool, flip: bool) -> Result<()> {
        // SAFETY: the camera is open (self)
        check("rc_cam_flip", unsafe { ffi::rc_cam_flip(mirror as i32, flip as i32) })
    }
}

/// Picture adjustments of the ISP, each 0..100, 50 = the tuning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Image {
    pub brightness: u32,
    pub contrast: u32,
    pub saturation: u32,
    pub hue: u32,
    pub sharpness: u32,
}

impl Drop for Camera<'_> {
    fn drop(&mut self) {
        // SAFETY: opened by Camera::open; the VPSS groups (borrowing it) are gone
        unsafe { ffi::rc_cam_close() };
    }
}

/// Fit of a VPSS output to its size.
#[derive(Debug, Clone, Copy)]
pub enum Fit {
    /// scale to the output size, aspect not kept
    Stretch,
    /// aspect kept, bars of this RGB colour (0xRRGGBB)
    Letterbox(u32),
}

/// One VPSS output channel.
#[derive(Debug, Clone, Copy)]
pub struct ChannelConfig {
    pub width: u32,
    pub height: u32,
    pub format: PixelFormat,
    pub fit: Fit,
    /// input crop (x, y, width, height), before the scaling
    pub crop: Option<(i32, i32, u32, u32)>,
}

/// A VPSS group fed by the camera, with its output channels. Shared by
/// reference between threads: the MPI frame calls are thread safe, one thread
/// per channel takes frames from the same group (F7, recamera-vicap "all").
pub struct VpssGroup<'c> {
    grp: i32,
    chns: Vec<i32>,
    started: bool,
    _cam: PhantomData<&'c ()>,
}

impl<'c> VpssGroup<'c> {
    /// Group `grp` on the camera output, channels 0.. as given, started.
    pub fn start(cam: &'c Camera<'_>, grp: i32, channels: &[ChannelConfig]) -> Result<VpssGroup<'c>> {
        let (w, h) = cam.size();
        let attr = ffi::VPSS_GRP_ATTR_S {
            u32MaxW: w,
            u32MaxH: h,
            enPixelFormat: ffi::PIXEL_FORMAT_NV21,
            stFrameRate: Default::default(),
            u8VpssDev: cam.sys.online() as u8,
        };
        // SAFETY: attr lives across the call
        check("VPSS_CreateGrp", unsafe { ffi::CVI_VPSS_CreateGrp(grp, &attr) })?;
        // from here on, Drop undoes what was done
        let mut g = VpssGroup { grp, chns: Vec::new(), started: false, _cam: PhantomData };
        // SAFETY: the group exists
        unsafe { ffi::CVI_VPSS_ResetGrp(grp) };

        for (i, c) in channels.iter().enumerate() {
            let chn = i as i32;
            let mut a = ffi::VPSS_CHN_ATTR_S {
                u32Width: c.width,
                u32Height: c.height,
                enVideoFormat: ffi::VIDEO_FORMAT_LINEAR,
                enPixelFormat: c.format.raw(),
                u32Depth: 1,
                ..Default::default()
            };
            if let Fit::Letterbox(color) = c.fit {
                a.stAspectRatio.enMode = ffi::ASPECT_RATIO_AUTO;
                a.stAspectRatio.bEnableBgColor = 1;
                a.stAspectRatio.u32BgColor = color;
            }
            // SAFETY: a lives across the call
            check("VPSS_SetChnAttr", unsafe { ffi::CVI_VPSS_SetChnAttr(grp, chn, &a) })?;
            if let Some((x, y, cw, ch)) = c.crop {
                let crop = ffi::VPSS_CROP_INFO_S {
                    bEnable: 1,
                    enCropCoordinate: ffi::VPSS_CROP_ABS_COOR,
                    stCropRect: ffi::RECT_S { s32X: x, s32Y: y, u32Width: cw, u32Height: ch },
                };
                // SAFETY: crop lives across the call
                check("VPSS_SetChnCrop", unsafe { ffi::CVI_VPSS_SetChnCrop(grp, chn, &crop) })?;
            }
            // SAFETY: the channel is configured
            check("VPSS_EnableChn", unsafe { ffi::CVI_VPSS_EnableChn(grp, chn) })?;
            g.chns.push(chn);
        }
        // Online, the ISP feeds the group directly; offline would need a
        // VI -> VPSS bind, which the daemon does not use.
        // SAFETY: the group and its channels exist
        check("VPSS_StartGrp", unsafe { ffi::CVI_VPSS_StartGrp(grp) })?;
        g.started = true;
        Ok(g)
    }

    /// The next frame of channel `chn`, waiting up to `timeout_ms`.
    pub fn frame(&self, chn: i32, timeout_ms: i32) -> Result<Frame<'_>> {
        let mut info = ffi::VIDEO_FRAME_INFO_S::zeroed();
        // SAFETY: info is a valid out-parameter
        check("VPSS_GetChnFrame", unsafe { ffi::CVI_VPSS_GetChnFrame(self.grp, chn, &mut info, timeout_ms) })?;
        Ok(Frame { grp: self.grp, chn, info, _g: PhantomData })
    }
}

impl Drop for VpssGroup<'_> {
    fn drop(&mut self) {
        // SAFETY: the group exists; its frames (borrowing it) are released
        unsafe {
            for &c in &self.chns {
                ffi::CVI_VPSS_DisableChn(self.grp, c);
            }
            if self.started {
                ffi::CVI_VPSS_StopGrp(self.grp);
            }
            ffi::CVI_VPSS_DestroyGrp(self.grp);
        }
    }
}

/// A frame taken from a VPSS channel; given back in `Drop`. The pixels stay
/// in the VB block (physical address for the TPU and the encoders).
pub struct Frame<'g> {
    grp: i32,
    chn: i32,
    info: ffi::VIDEO_FRAME_INFO_S,
    _g: PhantomData<&'g ()>,
}

// Released through the MPI from any thread.
unsafe impl Send for Frame<'_> {}

impl Frame<'_> {
    pub fn width(&self) -> u32 {
        self.info.stVFrame.u32Width
    }

    pub fn height(&self) -> u32 {
        self.info.stVFrame.u32Height
    }

    pub fn stride(&self, plane: usize) -> u32 {
        self.info.stVFrame.u32Stride[plane]
    }

    pub fn phys(&self, plane: usize) -> u64 {
        self.info.stVFrame.u64PhyAddr[plane]
    }

    pub fn len(&self, plane: usize) -> u32 {
        self.info.stVFrame.u32Length[plane]
    }

    /// all planes, contiguous from plane 0
    pub fn total_len(&self) -> u32 {
        self.info.stVFrame.u32Length.iter().sum()
    }

    /// presentation time stamp (µs)
    pub fn pts(&self) -> u64 {
        self.info.stVFrame.u64PTS
    }

    /// the raw frame, for the encoders and the TPU
    pub fn raw(&self) -> &ffi::VIDEO_FRAME_INFO_S {
        &self.info
    }

    /// The pixels for the CPU: all planes mapped cached and invalidated
    /// (what the hardware wrote, not stale cache lines).
    pub fn map(&self) -> Result<Mapping<'_>> {
        let (phys, len) = (self.phys(0), self.total_len());
        // SAFETY: the block belongs to this frame while it is held
        let va = unsafe { ffi::CVI_SYS_MmapCache(phys, len) };
        if va.is_null() {
            return Err(Error { op: "SYS_MmapCache", code: -1 });
        }
        // SAFETY: va maps [phys, phys + len)
        unsafe { ffi::CVI_SYS_IonInvalidateCache(phys, va, len) };
        Ok(Mapping { va: va.cast(), len, _f: PhantomData })
    }
}

impl Drop for Frame<'_> {
    fn drop(&mut self) {
        // SAFETY: taken by VpssGroup::frame and not released yet
        unsafe { ffi::CVI_VPSS_ReleaseChnFrame(self.grp, self.chn, &self.info) };
    }
}

/// The CPU view of a frame (all planes), unmapped in `Drop`.
pub struct Mapping<'f> {
    va: *mut u8,
    len: u32,
    _f: PhantomData<&'f ()>,
}

impl std::ops::Deref for Mapping<'_> {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        // SAFETY: va maps len bytes while the frame is held
        unsafe { std::slice::from_raw_parts(self.va, self.len as usize) }
    }
}

impl Drop for Mapping<'_> {
    fn drop(&mut self) {
        // SAFETY: mapped by Frame::map
        unsafe { ffi::CVI_SYS_Munmap(self.va.cast(), self.len) };
    }
}

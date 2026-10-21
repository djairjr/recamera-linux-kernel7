//! Hardware encoder channels (cvi_vc_driver, through recamera-cam): JPEG for
//! the MJPEG stream and snapshots, H.264 for RTSP/WebRTC.

use crate::{check, Frame, Result, Sys};
use sophgo_sys as ffi;
use std::marker::PhantomData;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Codec {
    /// quality 1..99
    Jpeg { quality: u32 },
    /// Baseline, CBR
    H264 { kbps: u32, gop: u32 },
}

/// An open encoder channel, closed in Drop (before the Sys it borrows).
pub struct Encoder<'s> {
    chn: i32,
    _sys: PhantomData<&'s ()>,
}

impl<'s> Encoder<'s> {
    pub fn open(_sys: &'s Sys, chn: i32, codec: Codec, width: u32, height: u32) -> Result<Encoder<'s>> {
        let (c, param, gop) = match codec {
            Codec::Jpeg { quality } => (ffi::RC_CODEC_JPEG, quality, 0),
            Codec::H264 { kbps, gop } => (ffi::RC_CODEC_H264, kbps, gop),
        };
        // SAFETY: SYS is open (borrowed)
        check("rc_venc_open", unsafe { ffi::rc_venc_open(chn, c, width, height, param, gop) })?;
        Ok(Encoder { chn, _sys: PhantomData })
    }

    /// H.264: the next frame is an IDR (a new viewer starts there).
    pub fn request_idr(&mut self) -> Result<()> {
        // SAFETY: an open channel
        check("rc_venc_request_idr", unsafe { ffi::rc_venc_request_idr(self.chn) })
    }

    /// Encode a frame into `out` (replaced). The buffer grows when a frame
    /// does not fit (that frame is lost: the encoder hands it out once).
    pub fn encode(&mut self, f: &Frame<'_>, timeout_ms: i32, out: &mut Vec<u8>) -> Result<()> {
        let mut len = 0u32;
        // SAFETY: the frame is held; out has `capacity` writable bytes
        let r = unsafe {
            ffi::rc_venc_encode(self.chn, f.raw(), timeout_ms, out.as_mut_ptr(), out.capacity() as u32, &mut len)
        };
        if r == 0 {
            // SAFETY: rc_venc_encode wrote len <= capacity bytes
            unsafe { out.set_len(len as usize) };
            return Ok(());
        }
        if len as usize > out.capacity() {
            out.clear();
            out.reserve(len as usize + len as usize / 4);
        }
        check("rc_venc_encode", r)
    }
}

impl Drop for Encoder<'_> {
    fn drop(&mut self) {
        // SAFETY: opened by Encoder::open
        unsafe { ffi::rc_venc_close(self.chn) };
    }
}

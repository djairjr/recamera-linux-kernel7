//! What the media threads hand to the network side: the last JPEG of the
//! stream and the detections of the last frame (newest only: memory stays
//! flat whatever the number of browsers), and the H.264 access units for
//! RTSP (a short broadcast: H.264 frames depend on the previous ones, a
//! client that falls behind waits for the next IDR). Each encoder only runs
//! while someone holds a [`Viewer`] of it.

use bytes::Bytes;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::{broadcast, watch};

/// One encoded H.264 frame (Annex B access unit).
pub struct Au {
    pub data: Bytes,
    /// capture time, microseconds
    pub pts_us: u64,
    /// an IDR: a viewer can start here
    pub key: bool,
}

/// access units kept for a client that falls behind (1 s at 30 fps)
const H264_BACKLOG: usize = 30;

pub struct Live {
    /// the last JPEG of the stream channel (empty: none yet)
    pub jpeg: watch::Sender<Bytes>,
    /// the detections of the last YOLO frame, as one JSON object
    pub dets: watch::Sender<Arc<str>>,
    /// MJPEG frames per second (stream.mjpeg_fps, changed live)
    pub mjpeg_fps: AtomicU32,
    viewers: AtomicUsize,
    /// H.264 access units for RTSP
    pub h264: broadcast::Sender<Arc<Au>>,
    /// SPS and PPS of the last IDR, for the SDP
    pub h264_params: Mutex<Option<(Bytes, Bytes)>>,
    /// a new viewer: the encoder makes the next frame an IDR
    pub idr_wanted: AtomicBool,
    h264_viewers: AtomicUsize,
}

impl Live {
    pub fn new(mjpeg_fps: u32) -> Arc<Live> {
        Arc::new(Live {
            jpeg: watch::channel(Bytes::new()).0,
            dets: watch::channel(Arc::from("{}")).0,
            mjpeg_fps: AtomicU32::new(mjpeg_fps),
            viewers: AtomicUsize::new(0),
            h264: broadcast::channel(H264_BACKLOG).0,
            h264_params: Mutex::new(None),
            idr_wanted: AtomicBool::new(false),
            h264_viewers: AtomicUsize::new(0),
        })
    }

    /// Someone wants H.264 while the guard lives; the next frame is an IDR.
    pub fn h264_viewer(self: &Arc<Live>) -> H264Viewer {
        self.h264_viewers.fetch_add(1, Ordering::Relaxed);
        self.idr_wanted.store(true, Ordering::Relaxed);
        H264Viewer(self.clone())
    }

    pub fn h264_viewers(&self) -> usize {
        self.h264_viewers.load(Ordering::Relaxed)
    }

    /// Someone wants JPEG frames while the returned guard lives.
    pub fn viewer(self: &Arc<Live>) -> Viewer {
        self.viewers.fetch_add(1, Ordering::Relaxed);
        Viewer(self.clone())
    }

    pub fn viewers(&self) -> usize {
        self.viewers.load(Ordering::Relaxed)
    }

    /// A JPEG of the stream now (the encoder runs for this one frame).
    pub async fn snapshot(self: &Arc<Live>) -> Option<Bytes> {
        let _viewer = self.viewer();
        let mut rx = self.jpeg.subscribe();
        rx.mark_unchanged();
        tokio::time::timeout(std::time::Duration::from_secs(3), rx.changed()).await.ok()?.ok()?;
        let j = rx.borrow().clone();
        (!j.is_empty()).then_some(j)
    }
}

pub struct Viewer(Arc<Live>);

impl Drop for Viewer {
    fn drop(&mut self) {
        self.0.viewers.fetch_sub(1, Ordering::Relaxed);
    }
}

pub struct H264Viewer(Arc<Live>);

impl Drop for H264Viewer {
    fn drop(&mut self) {
        self.0.h264_viewers.fetch_sub(1, Ordering::Relaxed);
    }
}

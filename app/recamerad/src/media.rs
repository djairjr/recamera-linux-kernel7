//! The media pipeline: camera -> VPSS group 0, online, two channels, as
//! validated in F7 (docs/multimedia.md §8), with YOLO on the
//! NPU over ch0 (stage A3):
//!
//! - ch0: 640x640 RGB packed, aspect kept with grey (114) bars: the YOLO input
//! - ch1: 1280x720 NV21, the 16:9 centre band of a 4:3 mode: the stream
//!
//! Detection (stage A4): boxes in stream coordinates (0..1, the picture the
//! web page shows), a box counts when its paws are in the ROI and it is big
//! enough; "target_arrived" after confirm_frames frames, "target_left" after
//! leave_after_s without it (recamera-detect). The thresholds, the ROI and
//! the target change live (Media::update_detection); the model and the
//! sensor mode restart the pipeline.
//!
//! Native threads (the MPI calls block): one owns the MPI objects, one per
//! channel takes the frames. Stopping is cooperative (a flag checked at
//! least every FRAME_TIMEOUT_MS); the teardown runs in Drop, in the order the
//! borrows impose. A killed run is cleaned by sophgo::reclaim at the next
//! start.

use crate::config::{self, Config};
use crate::events::Bus;
use crate::live::{Au, Live};
use bytes::Bytes;
use recamera_detect::{Change, Filter, Presence, StreamBox, View};
use serde::Serialize;
use sophgo::venc::{Codec, Encoder};
use sophgo::yolo::{Det, Yolo};
use sophgo::{Camera, ChannelConfig, Fit, PixelFormat, Pool, Reclaim, Sensor, Sys, VpssGroup};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const VPSS_GRP: i32 = 0;
pub const YOLO_W: u32 = 640;
pub const YOLO_H: u32 = 640;
pub const STREAM_W: u32 = 1280;
pub const STREAM_H: u32 = 720;
/// the YOLO letterbox grey (114, 114, 114)
const LETTERBOX: u32 = 0x727272;
/// blocks per VPSS output pool: 4 keeps 30 fps under the whole load (F7)
const VPSS_BLOCKS: u32 = 4;
const FRAME_TIMEOUT_MS: i32 = 500;
/// where a model given by name lives
const MODELS_DIR: &str = "/data/models";
const REPORT_EVERY: Duration = Duration::from_secs(10);
/// encoder channels (MaxVencChnNum = 3): 0 H.264 (RTSP), 1 JPEG
const H264_CHN: i32 = 0;
const JPEG_CHN: i32 = 1;

/// The running pipeline; `stop` tears it down.
pub struct Media {
    stop: Arc<AtomicBool>,
    params: Arc<Mutex<DetParams>>,
    image: Arc<Mutex<Picture>>,
    thread: Option<JoinHandle<()>>,
}

impl Media {
    pub fn start(cfg: &Config, bus: Bus, live: Arc<Live>) -> Media {
        let stop = Arc::new(AtomicBool::new(false));
        let params = Arc::new(Mutex::new(DetParams::from(&cfg.detection)));
        let image = Arc::new(Mutex::new(Picture::from(&cfg.camera)));
        let sensor = Sensor::from_name(&cfg.camera.sensor_mode).unwrap_or(Sensor::R1280x960);
        let det = cfg.detection.enabled.then(|| Detection { model: model_path(&cfg.detection.model) });
        let quality = cfg.stream.jpeg_quality;
        let h264 = Codec::H264 { kbps: cfg.rtsp.kbps, gop: cfg.rtsp.gop };
        let (st, pa, im) = (stop.clone(), params.clone(), image.clone());
        let thread = thread::Builder::new()
            .name("media".into())
            .spawn(move || {
                if let Err(e) = run(sensor, det, quality, h264, &pa, &im, &live, &st, &bus) {
                    crate::error!("media: {e}");
                    bus.emit("media_error", &serde_json::json!({ "error": e }));
                }
            })
            .map_err(|e| crate::error!("media thread: {e}"))
            .ok();
        Media { stop, params, image, thread }
    }

    /// Whether a new detection configuration needs a restart (model, on/off)
    /// or can be applied live (update_detection).
    pub fn needs_restart(old: &config::Detection, new: &config::Detection) -> bool {
        old.enabled != new.enabled || old.model != new.model
    }

    /// Picture adjustments and orientation, applied within 100 ms.
    pub fn update_camera(&self, c: &config::Camera) {
        if let Ok(mut p) = self.image.lock() {
            *p = Picture::from(c);
        }
    }

    /// Thresholds, ROI and target, applied from the next frame on.
    pub fn update_detection(&self, d: &config::Detection) {
        if let Ok(mut p) = self.params.lock() {
            *p = DetParams::from(d);
        }
    }

    /// Ask the pipeline to stop and wait for its teardown (blocking).
    pub fn stop(mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// A model given by name is in /data/models; a path is taken as it is.
pub fn model_path(m: &str) -> PathBuf {
    if m.contains('/') { PathBuf::from(m) } else { PathBuf::from(MODELS_DIR).join(m) }
}

/// The model of the YOLO channel (a change restarts the pipeline).
struct Detection {
    model: PathBuf,
}

/// The picture settings that change live.
#[derive(Clone, Copy, PartialEq)]
struct Picture {
    image: sophgo::Image,
    mirror: bool,
    flip: bool,
}

impl From<&config::Camera> for Picture {
    fn from(c: &config::Camera) -> Picture {
        Picture {
            image: sophgo::Image {
                brightness: c.brightness,
                contrast: c.contrast,
                saturation: c.saturation,
                hue: c.hue,
                sharpness: c.sharpness,
            },
            mirror: c.mirror,
            flip: c.flip,
        }
    }
}

fn apply_picture(cam: &Camera<'_>, p: &Picture) {
    if let Err(e) = cam.set_image(&p.image) {
        crate::warn!("media: picture: {e}");
    }
    if let Err(e) = cam.set_flip(p.mirror, p.flip) {
        crate::warn!("media: mirror/flip: {e}");
    }
}

/// The detection settings that change live.
#[derive(Clone, Copy, PartialEq)]
struct DetParams {
    target: u32,
    threshold: f32,
    nms: f32,
    filter: Filter,
    confirm: u32,
    leave_after: Duration,
}

impl From<&config::Detection> for DetParams {
    fn from(d: &config::Detection) -> DetParams {
        DetParams {
            target: d.target_class,
            threshold: d.threshold,
            nms: d.nms,
            filter: Filter { roi: d.roi, min_area: d.min_area },
            confirm: d.confirm_frames,
            leave_after: Duration::from_secs_f32(d.leave_after_s),
        }
    }
}

/// A detection as published: stream coordinates (0..1), `counts` = paws in
/// the ROI and big enough. f64 rounded, so the JSON says 0.851, not the f32
/// 0.8510000109672546.
#[derive(Serialize, Clone)]
struct DetBox {
    class: u32,
    score: f64,
    x1: f64,
    y1: f64,
    x2: f64,
    y2: f64,
    counts: bool,
}

impl DetBox {
    fn new(d: &Det, b: &StreamBox, counts: bool) -> DetBox {
        let r3 = |x: f32| (x as f64 * 1000.0).round() / 1000.0;
        DetBox { class: d.class, score: r3(d.score), x1: r3(b.x1), y1: r3(b.y1), x2: r3(b.x2), y2: r3(b.y2), counts }
    }
}

/// Class names from the model's metadata, <model>.json next to it
/// ("classes": [...], as the models in /data/models come); empty if none.
pub fn load_labels(model: &std::path::Path) -> Vec<String> {
    let j = std::fs::read(model.with_extension("json")).ok()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok());
    j.as_ref()
        .and_then(|v| v.get("classes"))
        .and_then(|c| c.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
        .unwrap_or_default()
}

#[derive(Serialize)]
struct ChannelInfo {
    width: u32,
    height: u32,
    format: &'static str,
}

#[derive(Default)]
struct Counters {
    frames: [AtomicU64; 2],
    timeouts: [AtomicU64; 2],
    /// frames through the NPU, their total time, frames with the target
    yolo_frames: AtomicU64,
    yolo_us: AtomicU64,
    target_frames: AtomicU64,
    /// JPEG frames encoded and their bytes
    jpeg_frames: AtomicU64,
    jpeg_bytes: AtomicU64,
    /// H.264 frames encoded and their bytes
    h264_frames: AtomicU64,
    h264_bytes: AtomicU64,
    /// the detections of the last frame
    last: Mutex<Vec<DetBox>>,
}

#[allow(clippy::too_many_arguments)]
fn run(sensor: Sensor, det: Option<Detection>, quality: u32, h264: Codec, params: &Mutex<DetParams>,
       image: &Mutex<Picture>, live: &Live, stop: &AtomicBool, bus: &Bus) -> Result<(), String> {
    match sophgo::reclaim() {
        Reclaim::Busy(pid) => return Err(format!("the camera is in use by pid {pid}")),
        Reclaim::Cleaned(0) => {}
        Reclaim::Cleaned(n) => crate::warn!("media: cleaned {n} MPI objects left by a killed run"),
    }

    let pools = [
        Pool { size: sophgo::buffer_size(YOLO_W, YOLO_H, PixelFormat::Rgb888), count: VPSS_BLOCKS },
        Pool { size: sophgo::buffer_size(STREAM_W, STREAM_H, PixelFormat::Nv21), count: VPSS_BLOCKS },
    ];
    let sys = Sys::open(&pools, true).map_err(|e| e.to_string())?;
    let cam = Camera::open(&sys, sensor).map_err(|e| e.to_string())?;
    let (w, h) = cam.size();
    let mut picture = image.lock().map(|p| *p).unwrap_or_else(|e| *e.into_inner());
    apply_picture(&cam, &picture);

    // a 4:3 mode: the 16:9 stream is its centre band, not a squeezed
    // picture (ch0, the YOLO input, keeps the whole frame)
    let crop = (w * 9 != h * 16).then(|| {
        let ch = (w * 9 / 16) & !1;
        (0, (((h - ch) / 2) & !1) as i32, w, ch)
    });
    let chans = [
        ChannelConfig {
            width: YOLO_W,
            height: YOLO_H,
            format: PixelFormat::Rgb888,
            fit: Fit::Letterbox(LETTERBOX),
            crop: None,
        },
        ChannelConfig { width: STREAM_W, height: STREAM_H, format: PixelFormat::Nv21, fit: Fit::Stretch, crop },
    ];
    let grp = VpssGroup::start(&cam, VPSS_GRP, &chans).map_err(|e| e.to_string())?;
    // the MJPEG/snapshot encoder: opened now, fed only while someone watches
    let mut jpeg = Encoder::open(&sys, JPEG_CHN, Codec::Jpeg { quality }, STREAM_W, STREAM_H)
        .map_err(|e| crate::error!("media: jpeg encoder: {e}; no MJPEG"))
        .ok();
    // the RTSP encoder: every frame while an RTSP client plays
    let mut h264 = Encoder::open(&sys, H264_CHN, h264, STREAM_W, STREAM_H)
        .map_err(|e| crate::error!("media: h264 encoder: {e}; no RTSP"))
        .ok();

    crate::info!("media: sensor {w}x{h}, ch0 {YOLO_W}x{YOLO_H} RGB, ch1 {STREAM_W}x{STREAM_H} NV21");
    bus.emit("media_started", &serde_json::json!({
        "sensor": format!("{w}x{h}"),
        "ch0": ChannelInfo { width: YOLO_W, height: YOLO_H, format: "rgb888" },
        "ch1": ChannelInfo { width: STREAM_W, height: STREAM_H, format: "nv21" },
        "stream_crop": crop,
    }));

    let counters = Counters::default();
    thread::scope(|s| {
        let (grp, counters) = (&grp, &counters);
        // ch0: YOLO on the NPU, straight from the frame (no copy)
        let mut yolo = det.as_ref().and_then(|d| open_yolo(d, params, bus));
        let labels = det.as_ref().map(|d| load_labels(&d.model)).unwrap_or_default();
        let view = View::new((w, h), (YOLO_W, YOLO_H), crop);
        let ch0 = thread::Builder::new().name("media-yolo".into()).spawn_scoped(s, move || {
            let mut dets: Vec<Det> = Vec::with_capacity(sophgo::yolo::MAX_DETECTIONS);
            let mut boxes: Vec<DetBox> = Vec::with_capacity(sophgo::yolo::MAX_DETECTIONS);
            let mut p = params.lock().map(|p| *p).unwrap_or_else(|e| *e.into_inner());
            let mut presence = Presence::new(p.confirm, p.leave_after);
            let label = |c: u32| labels.get(c as usize).cloned().unwrap_or_else(|| c.to_string());
            while !stop.load(Ordering::Acquire) {
                let Ok(f) = grp.frame(0, FRAME_TIMEOUT_MS) else {
                    counters.timeouts[0].fetch_add(1, Ordering::Relaxed);
                    continue;
                };
                counters.frames[0].fetch_add(1, Ordering::Relaxed);
                let Some(y) = yolo.as_mut() else { continue };

                // settings changed by a reload: a new target starts over
                let now_p = params.lock().map(|p| *p).unwrap_or(p);
                if now_p != p {
                    if now_p.target != p.target && presence.present() {
                        bus.emit("target_left", &serde_json::json!({
                            "class": p.target, "label": label(p.target), "reason": "target changed",
                        }));
                        presence = Presence::new(now_p.confirm, now_p.leave_after);
                    }
                    presence.set(now_p.confirm, now_p.leave_after);
                    p = now_p;
                }

                let t = Instant::now();
                if let Err(e) = y.detect_phys(f.phys(0), Some(p.target), p.threshold, p.nms, &mut dets) {
                    crate::error!("media: yolo: {e}; detection off");
                    yolo = None;
                    continue;
                }
                drop(f);
                counters.yolo_us.fetch_add(t.elapsed().as_micros() as u64, Ordering::Relaxed);
                counters.yolo_frames.fetch_add(1, Ordering::Relaxed);

                boxes.clear();
                let mut best: Option<usize> = None;
                for d in &dets {
                    let b = view.to_stream(d.x1, d.y1, d.x2, d.y2);
                    let counts = p.filter.accepts(&b);
                    if counts && best.is_none_or(|i| boxes[i].score < d.score as f64) {
                        best = Some(boxes.len());
                    }
                    boxes.push(DetBox::new(d, &b, counts));
                }
                if best.is_some() {
                    counters.target_frames.fetch_add(1, Ordering::Relaxed);
                }
                match presence.update(Instant::now(), best.is_some()) {
                    Some(Change::Arrived) => {
                        let b = &boxes[best.unwrap_or(0)];
                        crate::info!("media: target {} arrived (score {})", label(p.target), b.score);
                        bus.emit("target_arrived", &serde_json::json!({
                            "class": p.target,
                            "label": label(p.target),
                            "score": b.score,
                            "box": [b.x1, b.y1, b.x2, b.y2],
                            "count": boxes.iter().filter(|b| b.counts).count(),
                        }));
                    }
                    Some(Change::Left { stayed }) => {
                        let s = (stayed.as_secs_f64() * 10.0).round() / 10.0;
                        crate::info!("media: target {} left after {s} s", label(p.target));
                        bus.emit("target_left", &serde_json::json!({
                            "class": p.target, "label": label(p.target), "stayed_s": s,
                        }));
                    }
                    None => {}
                }
                if let Ok(mut l) = counters.last.lock() {
                    l.clone_from(&boxes);
                }
                // the boxes of every frame, for the page (only if it listens)
                if live.dets.receiver_count() > 0 {
                    let j = serde_json::json!({
                        "target": label(p.target),
                        "present": presence.present(),
                        "boxes": boxes,
                    });
                    live.dets.send_replace(Arc::from(j.to_string()));
                }
            }
        });
        // ch1: the stream; H.264 every frame while an RTSP client plays,
        // JPEG at mjpeg_fps while someone watches the page
        let (jpeg, h264) = (&mut jpeg, &mut h264);
        let ch1 = thread::Builder::new().name("media-ch1".into()).spawn_scoped(s, move || {
            let mut buf: Vec<u8> = Vec::with_capacity(256 * 1024);
            let mut hbuf: Vec<u8> = Vec::with_capacity(128 * 1024);
            let mut next = Instant::now();
            let t0 = Instant::now();
            while !stop.load(Ordering::Acquire) {
                let Ok(f) = grp.frame(1, FRAME_TIMEOUT_MS) else {
                    counters.timeouts[1].fetch_add(1, Ordering::Relaxed);
                    continue;
                };
                counters.frames[1].fetch_add(1, Ordering::Relaxed);
                if let Some(enc) = h264.as_mut().filter(|_| live.h264_viewers() > 0) {
                    if live.idr_wanted.swap(false, Ordering::Relaxed) {
                        let _ = enc.request_idr();
                    }
                    match enc.encode(&f, 1000, &mut hbuf) {
                        Ok(()) => {
                            let key = publish_params(live, &hbuf);
                            let pts_us = match f.pts() {
                                0 => t0.elapsed().as_micros() as u64,
                                p => p,
                            };
                            counters.h264_frames.fetch_add(1, Ordering::Relaxed);
                            counters.h264_bytes.fetch_add(hbuf.len() as u64, Ordering::Relaxed);
                            let _ = live.h264.send(Arc::new(Au { data: Bytes::copy_from_slice(&hbuf), pts_us, key }));
                        }
                        Err(e) => crate::debug!("media: h264: {e}"),
                    }
                }
                let Some(enc) = jpeg.as_mut() else { continue };
                // pace by the schedule, not by "now + period": frames come
                // every 33 ms, and "now + 66.7 ms" made 15 fps into 10
                let now = Instant::now();
                if live.viewers() == 0 || now + Duration::from_millis(8) < next {
                    continue;
                }
                let period = Duration::from_secs(1) / live.mjpeg_fps.load(Ordering::Relaxed).clamp(1, 30);
                next = if now.saturating_duration_since(next) > period { now + period } else { next + period };
                match enc.encode(&f, 1000, &mut buf) {
                    Ok(()) => {
                        counters.jpeg_frames.fetch_add(1, Ordering::Relaxed);
                        counters.jpeg_bytes.fetch_add(buf.len() as u64, Ordering::Relaxed);
                        live.jpeg.send_replace(Bytes::copy_from_slice(&buf));
                    }
                    Err(e) => crate::debug!("media: jpeg: {e}"),
                }
            }
        });
        if let Err(e) = ch0.and(ch1) {
            crate::error!("media: channel thread: {e}");
            stop.store(true, Ordering::Release);
        }

        // report the rates until asked to stop
        let mut last = Instant::now();
        let mut prev = [0u64; 2];
        while !stop.load(Ordering::Acquire) {
            thread::sleep(Duration::from_millis(100));
            if let Ok(p) = image.lock().map(|p| *p)
                && p != picture
            {
                picture = p;
                apply_picture(&cam, &picture);
            }
            let dt = last.elapsed();
            if dt < REPORT_EVERY {
                continue;
            }
            last = Instant::now();
            let now = [0, 1].map(|c| counters.frames[c].load(Ordering::Relaxed));
            let fps = [0, 1].map(|c| ((now[c] - prev[c]) as f64 / dt.as_secs_f64() * 10.0).round() / 10.0);
            prev = now;
            let timeouts = [0, 1].map(|c| counters.timeouts[c].load(Ordering::Relaxed));
            // the NPU over the same interval
            let yf = counters.yolo_frames.swap(0, Ordering::Relaxed);
            let yus = counters.yolo_us.swap(0, Ordering::Relaxed);
            let yolo_fps = (yf as f64 / dt.as_secs_f64() * 10.0).round() / 10.0;
            let yolo_ms = if yf > 0 { (yus as f64 / yf as f64 / 100.0).round() / 10.0 } else { 0.0 };
            let target_frames = counters.target_frames.swap(0, Ordering::Relaxed);
            let last = counters.last.lock().map(|l| l.clone()).unwrap_or_default();
            let jf = counters.jpeg_frames.swap(0, Ordering::Relaxed);
            let jb = counters.jpeg_bytes.swap(0, Ordering::Relaxed);
            let mjpeg_fps = (jf as f64 / dt.as_secs_f64() * 10.0).round() / 10.0;
            let jpeg_kb = if jf > 0 { jb / jf / 1024 } else { 0 };
            let hf = counters.h264_frames.swap(0, Ordering::Relaxed);
            let hb = counters.h264_bytes.swap(0, Ordering::Relaxed);
            let h264_fps = (hf as f64 / dt.as_secs_f64() * 10.0).round() / 10.0;
            let h264_kbps = (hb as f64 * 8.0 / dt.as_secs_f64() / 1000.0).round();
            bus.emit("media", &serde_json::json!({
                "ch0_fps": fps[0],
                "ch1_fps": fps[1],
                "frames": now,
                "timeouts": timeouts,
                "yolo_fps": yolo_fps,
                "yolo_ms": yolo_ms,
                "target_frames": target_frames,
                "detections": last,
                "mjpeg_fps": mjpeg_fps,
                "jpeg_kb": jpeg_kb,
                "viewers": live.viewers(),
                "h264_fps": h264_fps,
                "h264_kbps": h264_kbps,
                "rtsp_viewers": live.h264_viewers(),
            }));
        }
    });

    crate::info!("media: stopping");
    // the model (in the ch0 thread) is gone; grp, cam and sys drop here, in
    // that order
    Ok(())
}

/// Load the model for ch0; None (detection off, the stream goes on) when it
/// cannot be used.
fn open_yolo(d: &Detection, params: &Mutex<DetParams>, bus: &Bus) -> Option<Yolo> {
    let fail = |e: String| {
        crate::error!("media: {e}; detection off");
        bus.emit("media_error", &serde_json::json!({ "error": e }));
        None
    };
    let y = match Yolo::open(&d.model) {
        Ok(y) => y,
        Err(e) => return fail(e),
    };
    if y.input_size() != (YOLO_W, YOLO_H) {
        let (w, h) = y.input_size();
        return fail(format!("{}: input {w}x{h}, ch0 gives {YOLO_W}x{YOLO_H}", d.model.display()));
    }
    let p = params.lock().map(|p| *p).unwrap_or_else(|e| *e.into_inner());
    if p.target as usize >= y.classes() {
        return fail(format!("detection.target_class {}: the model has {} classes", p.target, y.classes()));
    }
    crate::info!("media: yolo {} ({} classes), target {} >= {}", d.model.display(), y.classes(), p.target,
                 p.threshold);
    Some(y)
}

/// Keep the SPS/PPS of an IDR access unit for the SDP; true if it is one.
fn publish_params(live: &Live, au: &[u8]) -> bool {
    use recamera_rtsp::{nal_type, nals, NAL_IDR, NAL_PPS, NAL_SPS};
    let units = nals(au);
    if !units.iter().any(|n| nal_type(n) == NAL_IDR) {
        return false;
    }
    let sps = units.iter().find(|n| nal_type(n) == NAL_SPS);
    let pps = units.iter().find(|n| nal_type(n) == NAL_PPS);
    if let (Some(s), Some(p), Ok(mut params)) = (sps, pps, live.h264_params.lock()) {
        *params = Some((Bytes::copy_from_slice(s), Bytes::copy_from_slice(p)));
    }
    true
}

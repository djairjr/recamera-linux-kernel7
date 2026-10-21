//! Detection logic of the reCamera, independent of the hardware (tested on
//! the host with `cargo test -p recamera-detect`):
//!
//! - [`View`]: from the YOLO input (the letterboxed 640x640 of the whole
//!   sensor frame) to coordinates relative to the stream image (0..1, the
//!   16:9 picture the web page shows), where the ROI is drawn;
//! - [`Roi`] and [`Filter`]: which detections count (the paws, the bottom
//!   centre of the box, inside the ROI; minimum box area);
//! - [`decode`]: the YOLO outputs (detection, pose, segment, classify) to
//!   boxes, keypoints, masks and classes;
//! - [`Presence`]: target arrival after N consecutive frames, departure
//!   after some seconds without it (owner's decision, 20/10/2026: only these
//!   two events, one rectangle as the area of interest).

pub mod decode;

use serde::{Deserialize, Serialize};
use std::time::{Duration, Instant};

/// A rectangle relative to the stream image: x, y of the top left corner,
/// width, height, all 0..1.
#[derive(Serialize, Deserialize, Clone, Copy, Debug, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Roi {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Roi {
    pub fn validate(&self) -> Result<(), String> {
        let ok = |v: f32| (0.0..=1.0).contains(&v);
        if !(ok(self.x) && ok(self.y) && self.w > 0.0 && self.h > 0.0 && self.x + self.w <= 1.0 + 1e-6
            && self.y + self.h <= 1.0 + 1e-6)
        {
            return Err(format!("roi {self:?}: x, y, w, h within 0..1, w and h > 0"));
        }
        Ok(())
    }

    pub fn contains(&self, u: f32, v: f32) -> bool {
        u >= self.x && u <= self.x + self.w && v >= self.y && v <= self.y + self.h
    }
}

/// A box relative to the stream image (0..1; a box can reach outside the
/// 16:9 band of a 4:3 sensor mode: below 0 or above 1).
#[derive(Serialize, Clone, Copy, Debug, PartialEq)]
pub struct StreamBox {
    pub x1: f32,
    pub y1: f32,
    pub x2: f32,
    pub y2: f32,
}

impl StreamBox {
    /// the paws: bottom centre
    pub fn foot(&self) -> (f32, f32) {
        ((self.x1 + self.x2) / 2.0, self.y2)
    }

    pub fn area(&self) -> f32 {
        (self.x2 - self.x1).max(0.0) * (self.y2 - self.y1).max(0.0)
    }
}

/// Geometry of the two VPSS outputs of the same sensor frame: the model
/// input (whole frame, aspect kept, centred bars) and the stream (a crop of
/// the frame, scaled to its size).
#[derive(Clone, Copy, Debug)]
pub struct View {
    scale: f32,
    pad_x: f32,
    pad_y: f32,
    crop: (f32, f32, f32, f32),
}

impl View {
    /// `sensor`: frame size; `model`: model input size; `crop`: the stream
    /// part of the frame (x, y, w, h in sensor pixels), None = whole frame.
    pub fn new(sensor: (u32, u32), model: (u32, u32), crop: Option<(i32, i32, u32, u32)>) -> View {
        let (sw, sh) = (sensor.0 as f32, sensor.1 as f32);
        let (mw, mh) = (model.0 as f32, model.1 as f32);
        let scale = (mw / sw).min(mh / sh);
        let crop = match crop {
            Some((x, y, w, h)) => (x as f32, y as f32, w as f32, h as f32),
            None => (0.0, 0.0, sw, sh),
        };
        View { scale, pad_x: (mw - sw * scale) / 2.0, pad_y: (mh - sh * scale) / 2.0, crop }
    }

    /// A box in model input pixels to stream coordinates.
    pub fn to_stream(&self, x1: f32, y1: f32, x2: f32, y2: f32) -> StreamBox {
        let (cx, cy, cw, ch) = self.crop;
        let u = |x: f32| ((x - self.pad_x) / self.scale - cx) / cw;
        let v = |y: f32| ((y - self.pad_y) / self.scale - cy) / ch;
        StreamBox { x1: u(x1), y1: v(y1), x2: u(x2), y2: v(y2) }
    }
}

/// Which detections count for the presence of the target.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Filter {
    pub roi: Option<Roi>,
    /// minimum box area, fraction of the stream image
    pub min_area: f32,
}

impl Filter {
    pub fn accepts(&self, b: &StreamBox) -> bool {
        if b.area() < self.min_area {
            return false;
        }
        match self.roi {
            Some(r) => {
                let (u, v) = b.foot();
                r.contains(u, v)
            }
            None => true,
        }
    }
}

/// A change of the target presence.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Change {
    Arrived,
    /// time from the arrival to the last frame with the target
    Left { stayed: Duration },
}

/// Arrival after `confirm` consecutive frames with the target; departure
/// after `leave_after` without it (a target turning away for a moment is not a
/// departure and a new arrival).
#[derive(Debug, Clone)]
pub struct Presence {
    confirm: u32,
    leave_after: Duration,
    hits: u32,
    present: bool,
    since: Option<Instant>,
    last_seen: Option<Instant>,
}

impl Presence {
    pub fn new(confirm: u32, leave_after: Duration) -> Presence {
        Presence { confirm: confirm.max(1), leave_after, hits: 0, present: false, since: None, last_seen: None }
    }

    /// New thresholds, keeping the current state.
    pub fn set(&mut self, confirm: u32, leave_after: Duration) {
        self.confirm = confirm.max(1);
        self.leave_after = leave_after;
    }

    pub fn present(&self) -> bool {
        self.present
    }

    /// One frame: `seen` = the target is in it (filtered).
    pub fn update(&mut self, now: Instant, seen: bool) -> Option<Change> {
        if seen {
            self.hits = self.hits.saturating_add(1);
            self.last_seen = Some(now);
            if !self.present && self.hits >= self.confirm {
                self.present = true;
                self.since = Some(now);
                return Some(Change::Arrived);
            }
            return None;
        }
        self.hits = 0;
        if self.present {
            let last = self.last_seen.unwrap_or(now);
            if now.duration_since(last) >= self.leave_after {
                self.present = false;
                let stayed = last.duration_since(self.since.unwrap_or(last));
                return Some(Change::Left { stayed });
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MS: Duration = Duration::from_millis(1);

    #[test]
    fn view_1280x960_letterbox_and_16_9_crop() {
        // 1280x960 -> 640x480 inside 640x640: 80 px bars top and bottom;
        // stream = rows 120..840 of the sensor
        let v = View::new((1280, 960), (640, 640), Some((0, 120, 1280, 720)));
        let b = v.to_stream(0.0, 80.0, 640.0, 560.0);
        // the whole sensor frame: wider than the 16:9 band vertically
        assert!((b.x1 - 0.0).abs() < 1e-5 && (b.x2 - 1.0).abs() < 1e-5);
        assert!((b.y1 - (-120.0 / 720.0)).abs() < 1e-5);
        assert!((b.y2 - (840.0 / 720.0)).abs() < 1e-5);
        // the stream band itself: model y 80 + 60 = 140 .. 80 + 420 = 500
        let s = v.to_stream(0.0, 140.0, 640.0, 500.0);
        assert!(s.y1.abs() < 1e-5 && (s.y2 - 1.0).abs() < 1e-5);
    }

    #[test]
    fn view_without_crop() {
        let v = View::new((1920, 1080), (640, 640), None);
        // 1920x1080 -> 640x360, bars of 140
        let b = v.to_stream(320.0, 140.0, 640.0, 500.0);
        assert!((b.x1 - 0.5).abs() < 1e-5 && b.y1.abs() < 1e-5 && (b.y2 - 1.0).abs() < 1e-5);
    }

    #[test]
    fn roi_counts_the_paws() {
        let f = Filter { roi: Some(Roi { x: 0.5, y: 0.5, w: 0.5, h: 0.5 }), min_area: 0.0 };
        // body outside, paws on the mat
        assert!(f.accepts(&StreamBox { x1: 0.6, y1: 0.1, x2: 0.8, y2: 0.7 }));
        // paws above the mat
        assert!(!f.accepts(&StreamBox { x1: 0.6, y1: 0.1, x2: 0.8, y2: 0.4 }));
        let small = Filter { roi: None, min_area: 0.05 };
        assert!(!small.accepts(&StreamBox { x1: 0.0, y1: 0.0, x2: 0.1, y2: 0.1 }));
        assert!(small.accepts(&StreamBox { x1: 0.0, y1: 0.0, x2: 0.3, y2: 0.3 }));
    }

    #[test]
    fn roi_validation() {
        assert!(Roi { x: 0.1, y: 0.1, w: 0.5, h: 0.5 }.validate().is_ok());
        assert!(Roi { x: 0.6, y: 0.1, w: 0.5, h: 0.5 }.validate().is_err());
        assert!(Roi { x: 0.1, y: 0.1, w: 0.0, h: 0.5 }.validate().is_err());
    }

    #[test]
    fn presence_confirms_and_tolerates_gaps() {
        let t0 = Instant::now();
        let mut p = Presence::new(3, 2000 * MS);
        assert_eq!(p.update(t0, true), None);
        assert_eq!(p.update(t0 + 40 * MS, true), None);
        assert_eq!(p.update(t0 + 80 * MS, true), Some(Change::Arrived));
        // a 1 s gap is not a departure
        assert_eq!(p.update(t0 + 600 * MS, false), None);
        assert_eq!(p.update(t0 + 1080 * MS, true), None);
        assert!(p.present());
        // 2 s without the target: left, stayed from 80 ms to 1080 ms
        assert_eq!(p.update(t0 + 2000 * MS, false), None);
        assert_eq!(p.update(t0 + 3100 * MS, false), Some(Change::Left { stayed: 1000 * MS }));
        assert!(!p.present());
    }

    #[test]
    fn presence_needs_consecutive_frames() {
        let t0 = Instant::now();
        let mut p = Presence::new(3, 2000 * MS);
        for i in 0..10u32 {
            // one frame with, one without: never confirmed
            assert_eq!(p.update(t0 + i * 40 * MS, i % 2 == 0), None);
        }
        assert!(!p.present());
    }
}

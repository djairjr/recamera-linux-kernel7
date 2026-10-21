//! YOLOv8/YOLO11 detection on the NPU: the .cvimodel does the network, the
//! decoding is here (the same arithmetic as the F7 C check,
//! external/package/sophgo-mpi/recamera-yolo.c, so both give the same boxes).
//!
//! Model input: UINT8 [1,H,W,3] RGB packed, mean 0 / scale 1 (the /255 is in
//! the model): a 640x640 VPSS channel letterboxed with grey 114. Outputs, per
//! stride 8/16/32: a box tensor [1,64,h,w] (4 sides x 16 DFL bins) and a class
//! tensor [1,C,h,w] (logits), INT8, real = int8 * qscale, NCHW.

use crate::nn::Model;
use std::path::Path;

const REG_MAX: usize = 16;
/// candidates kept before the NMS (in grid order, as the C check)
const MAX_CANDIDATES: usize = 300;
/// detections kept after the NMS
pub const MAX_DETECTIONS: usize = 64;

/// A detection, in model input pixels (letterboxed frame).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Det {
    pub x1: f32,
    pub y1: f32,
    pub x2: f32,
    pub y2: f32,
    pub score: f32,
    pub class: u32,
}

struct Scale {
    box_t: usize,
    cls_t: usize,
    h: usize,
    w: usize,
    stride: f32,
    box_q: f32,
    cls_q: f32,
}

pub struct Yolo {
    model: Model,
    scales: Vec<Scale>,
    classes: usize,
    in_w: u32,
    in_h: u32,
    cand: Vec<Det>,
}

impl Yolo {
    pub fn open(path: &Path) -> Result<Yolo, String> {
        let model = Model::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
        if model.inputs() != 1 {
            return Err(format!("{}: {} inputs, expected 1", path.display(), model.inputs()));
        }
        let ishape = model.input(0).shape();
        if ishape.len() != 4 || ishape[3] != 3 {
            return Err(format!("{}: input shape {ishape:?}, expected [1,H,W,3]", path.display()));
        }
        let (in_h, in_w) = (ishape[1] as u32, ishape[2] as u32);

        // pair the box (64 channels) and class tensors by their grid size
        let mut scales: Vec<(usize, usize, Option<(usize, f32)>, Option<(usize, f32)>)> = Vec::new();
        let mut classes = 0;
        for i in 0..model.outputs() {
            let t = model.output(i);
            let s = t.shape();
            if s.len() != 4 {
                return Err(format!("{}: output {i} shape {s:?}", path.display()));
            }
            let (c, h, w) = (s[1] as usize, s[2] as usize, s[3] as usize);
            let k = match scales.iter().position(|x| x.0 == h) {
                Some(k) => k,
                None => {
                    scales.push((h, w, None, None));
                    scales.len() - 1
                }
            };
            if c == 4 * REG_MAX {
                scales[k].2 = Some((i, t.qscale()));
            } else {
                scales[k].3 = Some((i, t.qscale()));
                classes = c;
            }
        }
        let scales = scales
            .into_iter()
            .map(|(h, w, b, c)| match (b, c) {
                (Some((box_t, box_q)), Some((cls_t, cls_q))) => Ok(Scale {
                    box_t,
                    cls_t,
                    h,
                    w,
                    stride: (in_h as usize / h) as f32,
                    box_q,
                    cls_q,
                }),
                _ => Err(format!("{}: not a YOLOv8/11 detection model (box+class per scale)", path.display())),
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Yolo { model, scales, classes, in_w, in_h, cand: Vec::with_capacity(MAX_CANDIDATES) })
    }

    pub fn input_size(&self) -> (u32, u32) {
        (self.in_w, self.in_h)
    }

    pub fn classes(&self) -> usize {
        self.classes
    }

    /// Detect on a frame already in memory at `paddr` (the model input size,
    /// RGB packed: a VPSS frame, no copy).
    pub fn detect_phys(&mut self, paddr: u64, target: Option<u32>, thr: f32, nms: f32, out: &mut Vec<Det>)
        -> Result<(), String> {
        self.model.set_input_phys(paddr).map_err(|e| e.to_string())?;
        self.model.forward().map_err(|e| e.to_string())?;
        self.decode(target, thr, nms, out);
        Ok(())
    }

    /// Detect on RGB packed pixels given by the CPU (a saved frame).
    pub fn detect_rgb(&mut self, rgb: &[u8], target: Option<u32>, thr: f32, nms: f32, out: &mut Vec<Det>)
        -> Result<(), String> {
        let buf = self.model.input_buffer();
        if rgb.len() != buf.len() {
            return Err(format!("input {} bytes, the model takes {}", rgb.len(), buf.len()));
        }
        buf.copy_from_slice(rgb);
        self.model.forward().map_err(|e| e.to_string())?;
        self.decode(target, thr, nms, out);
        Ok(())
    }

    /// Candidates above the threshold, then a greedy NMS per class; `out`
    /// gets the detections, best score first.
    fn decode(&mut self, target: Option<u32>, thr: f32, nms: f32, out: &mut Vec<Det>) {
        // class logit threshold: sigmoid(x) >= thr <=> x >= logit(thr)
        let lthr = (thr / (1.0 - thr)).ln();
        let (c0, c1) = match target {
            Some(t) => (t as usize, (t as usize + 1).min(self.classes)),
            None => (0, self.classes),
        };
        self.cand.clear();
        for sc in &self.scales {
            let bx = self.model.output(sc.box_t);
            let cl = self.model.output(sc.cls_t);
            let (bx, cl) = (bx.data_i8(), cl.data_i8());
            let plane = sc.h * sc.w;
            for i in 0..plane {
                let mut best = None;
                let mut bq = -128i8;
                for c in c0..c1 {
                    if cl[c * plane + i] > bq {
                        bq = cl[c * plane + i];
                        best = Some(c);
                    }
                }
                let l = bq as f32 * sc.cls_q;
                let Some(best) = best else { continue };
                if l < lthr || self.cand.len() == MAX_CANDIDATES {
                    continue;
                }
                let gx = (i % sc.w) as f32 + 0.5;
                let gy = (i / sc.w) as f32 + 0.5;
                let side = |k: usize| dfl(&bx[k * REG_MAX * plane + i..], plane, sc.box_q);
                let (dl, dt, dr, db) = (side(0), side(1), side(2), side(3));
                self.cand.push(Det {
                    x1: (gx - dl) * sc.stride,
                    y1: (gy - dt) * sc.stride,
                    x2: (gx + dr) * sc.stride,
                    y2: (gy + db) * sc.stride,
                    score: 1.0 / (1.0 + (-l).exp()),
                    class: best as u32,
                });
            }
        }

        self.cand.sort_by(|a, b| b.score.total_cmp(&a.score));
        out.clear();
        for c in &self.cand {
            if out.len() == MAX_DETECTIONS {
                break;
            }
            if !out.iter().any(|k| k.class == c.class && iou(k, c) > nms) {
                out.push(*c);
            }
        }
    }
}

/// Expected distance of one box side: softmax over the 16 DFL bins, which
/// lie `plane` apart starting at p[0].
fn dfl(p: &[i8], plane: usize, q: f32) -> f32 {
    let mut m = f32::MIN;
    for k in 0..REG_MAX {
        m = m.max(p[k * plane] as f32 * q);
    }
    let (mut sum, mut acc) = (0.0f32, 0.0f32);
    for k in 0..REG_MAX {
        let e = (p[k * plane] as f32 * q - m).exp();
        sum += e;
        acc += e * k as f32;
    }
    acc / sum
}

fn iou(a: &Det, b: &Det) -> f32 {
    let (x1, y1) = (a.x1.max(b.x1), a.y1.max(b.y1));
    let (x2, y2) = (a.x2.min(b.x2), a.y2.min(b.y2));
    let inter = (x2 - x1).max(0.0) * (y2 - y1).max(0.0);
    let ua = (a.x2 - a.x1) * (a.y2 - a.y1) + (b.x2 - b.x1) * (b.y2 - b.y1) - inter;
    if ua > 0.0 { inter / ua } else { 0.0 }
}

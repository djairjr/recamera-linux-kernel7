//! YOLOv8/YOLO11 models on the NPU: the .cvimodel does the network, the
//! decoding is here and in `recamera_detect::decode`.
//!
//! Model input: UINT8 [1,H,W,3] RGB packed, mean 0 / scale 1 (the /255 is in
//! the model), fed straight from a VPSS channel of that size. The task comes
//! from the outputs (`decode::layout`):
//!
//! - **split heads** (the Sophgo export of detection models): per stride
//!   8/16/32 a box tensor [1,64,h,w] (4 sides x 16 DFL bins) and a class
//!   tensor [1,C,h,w] (logits), INT8, real = int8 * qscale. The arithmetic is
//!   the same as the F7 C check (external/package/sophgo-mpi/recamera-yolo.c);
//! - **flat** (the Ultralytics export): one [1,C,N] output, already decoded,
//!   for detection, pose (K keypoints) and segment (+ the [1,32,h,w]
//!   prototypes), float or INT8;
//! - **classify**: [1,nc] probabilities.

use crate::nn::{Model, Tensor};
use recamera_detect::decode::{self, Extra, Layout};
use std::path::Path;

pub use recamera_detect::decode::{Det, MAX_DETECTIONS, Mask, Results};

const REG_MAX: usize = 16;
/// classes reported by a classification model
const TOP_K: usize = 5;

/// What a model does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Task {
    Detect,
    Pose,
    Segment,
    Classify,
}

impl Task {
    pub fn name(self) -> &'static str {
        match self {
            Task::Detect => "detect",
            Task::Pose => "pose",
            Task::Segment => "segment",
            Task::Classify => "classify",
        }
    }
}

/// What the model's .json says, when it says it.
#[derive(Debug, Default, Clone, Copy)]
pub struct Hints<'a> {
    /// the length of its "classes"
    pub classes: Option<usize>,
    /// "task": detect, pose, segment or classify
    pub task: Option<&'a str>,
    /// "kpt_shape": [keypoints, 2 or 3]
    pub kpt_shape: Option<(usize, usize)>,
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

enum Kind {
    Split(Vec<Scale>),
    Flat { main: usize, n: usize, nc: usize, extra: Extra, proto: Option<(usize, usize, usize)> },
    Classify { main: usize, nc: usize },
}

pub struct Yolo {
    model: Model,
    kind: Kind,
    classes: usize,
    in_w: u32,
    in_h: u32,
    cand: Vec<Det>,
}

/// The values of an output tensor, float or INT8.
enum Vals<'a> {
    F32(&'a [f32]),
    I8(&'a [i8], f32),
}

impl Vals<'_> {
    fn of<'t>(t: &'t Tensor<'_>) -> Vals<'t> {
        let elems: usize = t.shape().iter().map(|&d| d.max(1) as usize).product();
        if elems > 0 && t.size() == elems * 4 { Vals::F32(t.data_f32()) } else { Vals::I8(t.data_i8(), t.qscale()) }
    }

    #[inline]
    fn get(&self, i: usize) -> f32 {
        match self {
            Vals::F32(v) => v[i],
            Vals::I8(v, q) => v[i] as f32 * q,
        }
    }
}

impl Yolo {
    pub fn open(path: &Path, hints: Hints<'_>) -> Result<Yolo, String> {
        let err = |e: String| format!("{}: {e}", path.display());
        let model = Model::open(path).map_err(|e| err(e.to_string()))?;
        if model.inputs() != 1 {
            return Err(err(format!("{} inputs, expected 1", model.inputs())));
        }
        let ishape = model.input(0).shape();
        if ishape.len() != 4 || ishape[3] != 3 {
            return Err(err(format!("input shape {ishape:?}, expected [1,H,W,3]")));
        }
        let (in_h, in_w) = (ishape[1] as u32, ishape[2] as u32);
        let shapes: Vec<Vec<i32>> = (0..model.outputs()).map(|i| model.output(i).shape()).collect();
        let (kind, classes) = match decode::layout(&shapes, hints.classes, hints.task, hints.kpt_shape).map_err(&err)? {
            Layout::Split => {
                let (scales, classes) = split_scales(&model, in_h).map_err(&err)?;
                (Kind::Split(scales), classes)
            }
            Layout::Flat { main, n, nc, extra, proto, .. } => {
                let proto = proto.map(|p| {
                    let s = &shapes[p];
                    (p, s[2] as usize, s[3] as usize)
                });
                (Kind::Flat { main, n, nc, extra, proto }, nc)
            }
            Layout::Classify { main, nc } => (Kind::Classify { main, nc }, nc),
        };
        let y = Yolo { model, kind, classes, in_w, in_h, cand: Vec::with_capacity(decode::MAX_CANDIDATES) };
        if let Some(t) = hints.task
            && t != y.task().name()
        {
            return Err(err(format!("the .json says {t}, the outputs are those of {}", y.task().name())));
        }
        Ok(y)
    }

    pub fn input_size(&self) -> (u32, u32) {
        (self.in_w, self.in_h)
    }

    pub fn classes(&self) -> usize {
        self.classes
    }

    pub fn task(&self) -> Task {
        match &self.kind {
            Kind::Split(_) => Task::Detect,
            Kind::Flat { extra: Extra::None, .. } => Task::Detect,
            Kind::Flat { extra: Extra::Pose { .. }, .. } => Task::Pose,
            Kind::Flat { extra: Extra::Segment { .. }, .. } => Task::Segment,
            Kind::Classify { .. } => Task::Classify,
        }
    }

    /// keypoints per detection (pose), else 0
    pub fn keypoints(&self) -> usize {
        match self.kind {
            Kind::Flat { extra: Extra::Pose { k, .. }, .. } => k,
            _ => 0,
        }
    }

    /// Run the model on a frame already in memory at `paddr` (the model
    /// input size, RGB packed: a VPSS frame, no copy).
    pub fn infer_phys(&mut self, paddr: u64) -> Result<(), String> {
        self.model.set_input_phys(paddr).map_err(|e| e.to_string())?;
        self.model.forward().map_err(|e| e.to_string())
    }

    /// Run the model on RGB packed pixels given by the CPU (a saved frame).
    pub fn infer_rgb(&mut self, rgb: &[u8]) -> Result<(), String> {
        let buf = self.model.input_buffer();
        if rgb.len() != buf.len() {
            return Err(format!("input {} bytes, the model takes {}", rgb.len(), buf.len()));
        }
        buf.copy_from_slice(rgb);
        self.model.forward().map_err(|e| e.to_string())
    }

    /// The results of the last inference. `target` limits the classes looked
    /// at; `masks` computes the segment masks (CPU work: only when shown).
    pub fn decode(&mut self, target: Option<u32>, thr: f32, iou: f32, masks: bool, out: &mut Results) {
        out.clear();
        match &self.kind {
            Kind::Split(scales) => decode_split(&self.model, scales, self.classes, target, thr, iou, &mut self.cand,
                                                out),
            Kind::Flat { main, n, nc, extra, proto } => {
                let t = self.model.output(*main);
                let v = Vals::of(&t);
                let n = *n;
                decode::decode_flat(|c, i| v.get(c * n + i), n, *nc, *extra, target, thr, iou, &mut self.cand, out);
                if let (Extra::Segment { coeffs }, Some((p, ph, pw))) = (extra, proto) {
                    let pt = self.model.output(*p);
                    let pv = Vals::of(&pt);
                    let (ph, pw, nc) = (*ph, *pw, *nc);
                    for d in &out.dets {
                        out.masks.push(masks.then(|| {
                            let a = d.anchor as usize;
                            decode::mask(d, |j| v.get((4 + nc + j) * n + a), *coeffs,
                                         |pp, y, x| pv.get((pp * ph + y) * pw + x), pw, ph, self.in_w, self.in_h)
                        }));
                    }
                }
            }
            Kind::Classify { main, nc } => {
                let t = self.model.output(*main);
                let v = Vals::of(&t);
                let nc = *nc;
                // probabilities as they come; logits (any value outside 0..1) get a softmax
                let raw: Vec<f32> = (0..nc).map(|i| v.get(i)).collect();
                let probs = if raw.iter().all(|p| (0.0..=1.0).contains(p)) {
                    raw
                } else {
                    let m = raw.iter().copied().fold(f32::MIN, f32::max);
                    let e: Vec<f32> = raw.iter().map(|x| (x - m).exp()).collect();
                    let s: f32 = e.iter().sum();
                    e.into_iter().map(|x| x / s).collect()
                };
                out.top = decode::top_k(|i| probs[i], nc, TOP_K);
                if let Some(t) = target
                    && !out.top.iter().any(|&(c, _)| c == t)
                    && (t as usize) < nc
                {
                    out.top.push((t, probs[t as usize]));
                }
            }
        }
    }

    /// Detection only, as before: infer and decode boxes (recamera-yolo).
    pub fn detect_rgb(&mut self, rgb: &[u8], target: Option<u32>, thr: f32, iou: f32, out: &mut Results)
        -> Result<(), String> {
        self.infer_rgb(rgb)?;
        self.decode(target, thr, iou, false, out);
        Ok(())
    }
}

/// Pair the box (64 channels) and class tensors by their grid size.
fn split_scales(model: &Model, in_h: u32) -> Result<(Vec<Scale>, usize), String> {
    let mut scales: Vec<(usize, usize, Option<(usize, f32)>, Option<(usize, f32)>)> = Vec::new();
    let mut classes = 0;
    for i in 0..model.outputs() {
        let t = model.output(i);
        let s = t.shape();
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
            (Some((box_t, box_q)), Some((cls_t, cls_q))) => {
                Ok(Scale { box_t, cls_t, h, w, stride: (in_h as usize / h) as f32, box_q, cls_q })
            }
            _ => Err("not a YOLOv8/11 detection model (box+class per scale)".to_string()),
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok((scales, classes))
}

/// Candidates above the threshold, then the NMS; `out.dets` gets the
/// detections, best score first.
#[allow(clippy::too_many_arguments)]
fn decode_split(model: &Model, scales: &[Scale], classes: usize, target: Option<u32>, thr: f32, iou: f32,
                cand: &mut Vec<Det>, out: &mut Results) {
    // class logit threshold: sigmoid(x) >= thr <=> x >= logit(thr)
    let lthr = (thr / (1.0 - thr)).ln();
    let (c0, c1) = match target {
        Some(t) => (t as usize, (t as usize + 1).min(classes)),
        None => (0, classes),
    };
    cand.clear();
    for sc in scales {
        let bx = model.output(sc.box_t);
        let cl = model.output(sc.cls_t);
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
            if l < lthr || cand.len() == decode::MAX_CANDIDATES {
                continue;
            }
            let gx = (i % sc.w) as f32 + 0.5;
            let gy = (i / sc.w) as f32 + 0.5;
            let side = |k: usize| dfl(&bx[k * REG_MAX * plane + i..], plane, sc.box_q);
            let (dl, dt, dr, db) = (side(0), side(1), side(2), side(3));
            cand.push(Det {
                x1: (gx - dl) * sc.stride,
                y1: (gy - dt) * sc.stride,
                x2: (gx + dr) * sc.stride,
                y2: (gy + db) * sc.stride,
                score: 1.0 / (1.0 + (-l).exp()),
                class: best as u32,
                anchor: i as u32,
            });
        }
    }
    decode::nms(cand, iou, &mut out.dets);
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

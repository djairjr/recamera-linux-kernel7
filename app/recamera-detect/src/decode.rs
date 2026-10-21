//! Decoding of YOLOv8/YOLO11 outputs, independent of the NPU (the tensors
//! come in as accessors), so it is tested on the host.
//!
//! - [`decode_flat`]: the Ultralytics export ("output0", `[1, C, N]`, one
//!   column per anchor, already decoded: box centre and size in input pixels,
//!   class scores after the sigmoid), for detection (`C = 4 + nc`), pose
//!   (`+ K x 2|3` keypoints) and segment (`+ 32` mask coefficients);
//! - [`mask`]: a segment mask from the coefficients and the prototypes;
//! - [`top_k`]: the classes of a classification model;
//! - [`nms`]: greedy per-class suppression, shared with the split-head
//!   decoder of the sophgo crate.

/// A detection, in model input pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Det {
    pub x1: f32,
    pub y1: f32,
    pub x2: f32,
    pub y2: f32,
    pub score: f32,
    pub class: u32,
    /// the column (anchor) it came from, for its keypoints or mask coefficients
    pub anchor: u32,
}

/// What the columns hold after the box and the class scores.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Extra {
    None,
    /// `k` keypoints of `dims` values each (x, y and, with 3, visibility)
    Pose { k: usize, dims: usize },
    /// mask coefficients
    Segment { coeffs: usize },
}

/// The results of one frame.
#[derive(Debug, Default)]
pub struct Results {
    /// best score first, at most [`MAX_DETECTIONS`]
    pub dets: Vec<Det>,
    /// pose: `k` keypoints per detection, in the order of `dets`
    /// (x, y in model pixels, visibility 0..1; 1 when the model has none)
    pub kpts: Vec<[f32; 3]>,
    pub k: usize,
    /// segment: one mask per detection, in the order of `dets` (None when not asked)
    pub masks: Vec<Option<Mask>>,
    /// classify: the best classes, best first
    pub top: Vec<(u32, f32)>,
}

impl Results {
    pub fn clear(&mut self) {
        self.dets.clear();
        self.kpts.clear();
        self.masks.clear();
        self.top.clear();
    }
}

/// A segment mask over a rectangle of the prototype grid: `w x h` cells
/// from (`x`, `y`), one bit per cell, rows packed from the most significant
/// bit, each row starting on a new byte. `cell` is the size of a cell in
/// model pixels.
#[derive(Debug, Clone, PartialEq)]
pub struct Mask {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
    pub cell: f32,
    pub bits: Vec<u8>,
}

impl Mask {
    pub fn get(&self, cx: u32, cy: u32) -> bool {
        let stride = self.w.div_ceil(8) as usize;
        self.bits[cy as usize * stride + cx as usize / 8] & (0x80 >> (cx % 8)) != 0
    }
}

/// candidates kept before the NMS
pub const MAX_CANDIDATES: usize = 300;
/// detections kept after the NMS
pub const MAX_DETECTIONS: usize = 64;

/// Decode an Ultralytics `[1, C, N]` output. `at(c, i)` is the value of
/// channel `c` for anchor `i`; `nc` the number of classes; `target` limits
/// the classes looked at. `cand` is scratch space (kept to avoid allocating).
#[allow(clippy::too_many_arguments)]
pub fn decode_flat(at: impl Fn(usize, usize) -> f32, n: usize, nc: usize, extra: Extra, target: Option<u32>,
                   thr: f32, iou: f32, cand: &mut Vec<Det>, out: &mut Results) {
    out.clear();
    let (c0, c1) = match target {
        Some(t) => (t as usize, (t as usize + 1).min(nc)),
        None => (0, nc),
    };
    cand.clear();
    for i in 0..n {
        let mut best = (0usize, f32::MIN);
        for c in c0..c1 {
            let s = at(4 + c, i);
            if s > best.1 {
                best = (c, s);
            }
        }
        if best.1 < thr || cand.len() == MAX_CANDIDATES {
            continue;
        }
        let (cx, cy, w, h) = (at(0, i), at(1, i), at(2, i), at(3, i));
        cand.push(Det {
            x1: cx - w / 2.0,
            y1: cy - h / 2.0,
            x2: cx + w / 2.0,
            y2: cy + h / 2.0,
            score: best.1,
            class: best.0 as u32,
            anchor: i as u32,
        });
    }
    nms(cand, iou, &mut out.dets);
    if let Extra::Pose { k, dims } = extra {
        out.k = k;
        for d in &out.dets {
            let i = d.anchor as usize;
            for j in 0..k {
                let base = 4 + nc + j * dims;
                let v = if dims >= 3 { at(base + 2, i) } else { 1.0 };
                out.kpts.push([at(base, i), at(base + 1, i), v]);
            }
        }
    }
}

/// Greedy NMS per class: `cand` sorted by score, `out` gets the survivors.
pub fn nms(cand: &mut [Det], iou_thr: f32, out: &mut Vec<Det>) {
    cand.sort_by(|a, b| b.score.total_cmp(&a.score));
    out.clear();
    for c in cand.iter() {
        if out.len() == MAX_DETECTIONS {
            break;
        }
        if !out.iter().any(|k| k.class == c.class && iou(k, c) > iou_thr) {
            out.push(*c);
        }
    }
}

pub fn iou(a: &Det, b: &Det) -> f32 {
    let (x1, y1) = (a.x1.max(b.x1), a.y1.max(b.y1));
    let (x2, y2) = (a.x2.min(b.x2), a.y2.min(b.y2));
    let inter = (x2 - x1).max(0.0) * (y2 - y1).max(0.0);
    let ua = (a.x2 - a.x1) * (a.y2 - a.y1) + (b.x2 - b.x1) * (b.y2 - b.y1) - inter;
    if ua > 0.0 { inter / ua } else { 0.0 }
}

/// The mask of a detection: sigmoid(coefficients · prototypes) > 0.5 over
/// the cells its box covers. `proto(p, y, x)` is prototype `p` at cell (x,
/// y) of a `pw x ph` grid covering the `in_w x in_h` model input.
#[allow(clippy::too_many_arguments)]
pub fn mask(d: &Det, coeff: impl Fn(usize) -> f32, coeffs: usize, proto: impl Fn(usize, usize, usize) -> f32,
            pw: usize, ph: usize, in_w: u32, in_h: u32) -> Mask {
    let cell = in_w as f32 / pw as f32;
    let celly = in_h as f32 / ph as f32;
    let cl = |v: f32, m: usize| (v.max(0.0) as usize).min(m);
    let (x0, x1) = (cl((d.x1 / cell).floor(), pw), cl((d.x2 / cell).ceil(), pw));
    let (y0, y1) = (cl((d.y1 / celly).floor(), ph), cl((d.y2 / celly).ceil(), ph));
    let (w, h) = (x1.saturating_sub(x0), y1.saturating_sub(y0));
    let stride = w.div_ceil(8);
    let mut bits = vec![0u8; stride * h];
    let c: Vec<f32> = (0..coeffs).map(&coeff).collect();
    for yy in 0..h {
        for xx in 0..w {
            let mut s = 0.0f32;
            for (p, cp) in c.iter().enumerate() {
                s += cp * proto(p, y0 + yy, x0 + xx);
            }
            // sigmoid(s) > 0.5 <=> s > 0
            if s > 0.0 {
                bits[yy * stride + xx / 8] |= 0x80 >> (xx % 8);
            }
        }
    }
    Mask { x: x0 as u32, y: y0 as u32, w: w as u32, h: h as u32, cell, bits }
}

/// The `k` best of `n` class probabilities.
pub fn top_k(p: impl Fn(usize) -> f32, n: usize, k: usize) -> Vec<(u32, f32)> {
    let mut v: Vec<(u32, f32)> = (0..n).map(|i| (i as u32, p(i))).collect();
    v.sort_by(|a, b| b.1.total_cmp(&a.1));
    v.truncate(k);
    v
}

/// How a model's outputs are laid out, which says its task.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Layout {
    /// box + class tensors per stride (the Sophgo export: raw DFL, INT8)
    Split,
    /// one `[1, C, N]` output; `nc` classes; segment adds the prototypes output
    Flat { main: usize, c: usize, n: usize, nc: usize, extra: Extra, proto: Option<usize> },
    /// `[1, nc]` probabilities
    Classify { main: usize, nc: usize },
}

/// The layout of a model from the shapes of its outputs (NCHW, trailing 1s
/// allowed), as the tensors come out of a cvimodel. `nc` is the number of
/// classes from the model's .json, when known; `hint` its "task" field and
/// `kpt_shape` its "kpt_shape".
pub fn layout(shapes: &[Vec<i32>], nc: Option<usize>, hint: Option<&str>, kpt_shape: Option<(usize, usize)>)
    -> Result<Layout, String> {
    // drop leading batch and trailing 1s: [1, C, N, 1] -> [C, N]
    let core = |s: &Vec<i32>| -> Vec<usize> {
        let mut v: Vec<usize> = s.iter().map(|&d| d.max(0) as usize).collect();
        while v.len() > 1 && v.last() == Some(&1) {
            v.pop();
        }
        if v.len() > 1 && v[0] == 1 {
            v.remove(0);
        }
        v
    };
    let cores: Vec<Vec<usize>> = shapes.iter().map(core).collect();
    // the Sophgo split heads: pairs of 4D outputs, one with 64 channels per grid
    if cores.len() >= 2 && cores.len() % 2 == 0 && cores.iter().all(|c| c.len() == 3)
        && cores.iter().filter(|c| c[0] == 64).count() == cores.len() / 2
    {
        return Ok(Layout::Split);
    }
    if cores.len() == 1 && cores[0].len() == 1 {
        return Ok(Layout::Classify { main: 0, nc: cores[0][0] });
    }
    let main = cores.iter().position(|c| c.len() == 2)
        .ok_or_else(|| format!("unknown output layout {shapes:?}"))?;
    let (c, n) = (cores[main][0], cores[main][1]);
    let proto = cores.iter().position(|c| c.len() == 3);
    let nc = match nc {
        Some(nc) => nc,
        // without the class list: detection, all channels after the box are classes
        None if proto.is_none() && kpt_shape.is_none() && hint.is_none_or(|h| h == "detect") => c.saturating_sub(4),
        None => return Err("the model's .json must list its classes".into()),
    };
    if c < 4 + nc {
        return Err(format!("output has {c} channels, fewer than 4 + {nc} classes"));
    }
    let rest = c - 4 - nc;
    let extra = match (proto, rest) {
        (Some(p), r) => {
            if cores[p][0] != r {
                return Err(format!("{r} mask coefficients but {} prototypes", cores[p][0]));
            }
            Extra::Segment { coeffs: r }
        }
        (None, 0) => Extra::None,
        (None, r) => {
            let (k, dims) = match kpt_shape {
                Some((k, d)) if k * d == r => (k, d),
                Some((k, d)) => return Err(format!("kpt_shape [{k}, {d}] but {r} keypoint channels")),
                None if hint == Some("pose") || r % 3 == 0 || r % 2 == 0 => {
                    if r % 3 == 0 { (r / 3, 3) } else { (r / 2, 2) }
                }
                None => return Err(format!("{r} channels after the classes: not pose, not segment")),
            };
            Extra::Pose { k, dims }
        }
    };
    Ok(Layout::Flat { main, c, n, nc, extra, proto })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// a [C, N] output as a Vec, filled by (c, i) -> value
    fn grid(c: usize, n: usize, f: impl Fn(usize, usize) -> f32) -> Vec<f32> {
        let mut v = vec![0.0; c * n];
        for cc in 0..c {
            for i in 0..n {
                v[cc * n + i] = f(cc, i);
            }
        }
        v
    }

    #[test]
    fn layouts_of_the_factory_models() {
        let split = vec![vec![1, 64, 80, 80], vec![1, 80, 80, 80], vec![1, 64, 40, 40], vec![1, 80, 40, 40],
                         vec![1, 64, 20, 20], vec![1, 80, 20, 20]];
        assert_eq!(layout(&split, Some(80), None, None), Ok(Layout::Split));
        let pose = vec![vec![1, 56, 8400, 1]];
        assert_eq!(layout(&pose, Some(1), None, None).unwrap(),
                   Layout::Flat { main: 0, c: 56, n: 8400, nc: 1, extra: Extra::Pose { k: 17, dims: 3 }, proto: None });
        let seg = vec![vec![1, 116, 8400, 1], vec![1, 32, 160, 160]];
        assert_eq!(layout(&seg, Some(80), None, None).unwrap(),
                   Layout::Flat { main: 0, c: 116, n: 8400, nc: 80, extra: Extra::Segment { coeffs: 32 }, proto: Some(1) });
        let cls = vec![vec![1, 1000, 1, 1]];
        assert_eq!(layout(&cls, Some(1000), None, None), Ok(Layout::Classify { main: 0, nc: 1000 }));
        // a flat detection model without a .json
        let det = vec![vec![1, 84, 8400]];
        assert!(matches!(layout(&det, None, None, None), Ok(Layout::Flat { nc: 80, extra: Extra::None, .. })));
    }

    #[test]
    fn dog_pose_24_keypoints() {
        // Ultralytics dog-pose: 1 class, 24 keypoints x 3
        let s = vec![vec![1, 4 + 1 + 72, 8400, 1]];
        assert!(matches!(layout(&s, Some(1), None, None),
                         Ok(Layout::Flat { extra: Extra::Pose { k: 24, dims: 3 }, .. })));
        // kpt_shape from the .json wins
        assert!(matches!(layout(&s, Some(1), None, Some((36, 2))),
                         Ok(Layout::Flat { extra: Extra::Pose { k: 36, dims: 2 }, .. })));
        assert!(layout(&s, Some(1), None, Some((17, 3))).is_err());
    }

    #[test]
    fn flat_pose_decodes_box_and_keypoints() {
        let (n, nc, k) = (6, 1, 2);
        let c = 4 + nc + 3 * k;
        // anchor 2: a box at (100, 200) 40x60 with score 0.9, keypoints (90, 180, 0.8) and (110, 220, 0.3)
        let v = grid(c, n, |cc, i| match (cc, i) {
            (0, 2) => 100.0, (1, 2) => 200.0, (2, 2) => 40.0, (3, 2) => 60.0, (4, 2) => 0.9,
            (5, 2) => 90.0, (6, 2) => 180.0, (7, 2) => 0.8, (8, 2) => 110.0, (9, 2) => 220.0, (10, 2) => 0.3,
            (4, _) => 0.1,
            _ => 0.0,
        });
        let mut out = Results::default();
        let mut cand = Vec::new();
        decode_flat(|cc, i| v[cc * n + i], n, nc, Extra::Pose { k, dims: 3 }, None, 0.5, 0.45, &mut cand, &mut out);
        assert_eq!(out.dets.len(), 1);
        let d = out.dets[0];
        assert_eq!((d.x1, d.y1, d.x2, d.y2, d.anchor), (80.0, 170.0, 120.0, 230.0, 2));
        assert_eq!(out.kpts, vec![[90.0, 180.0, 0.8], [110.0, 220.0, 0.3]]);
    }

    #[test]
    fn nms_keeps_the_best_of_overlapping_boxes() {
        let b = |x: f32, s: f32, class: u32| Det { x1: x, y1: 0.0, x2: x + 10.0, y2: 10.0, score: s, class, anchor: 0 };
        let mut cand = vec![b(0.0, 0.6, 0), b(1.0, 0.9, 0), b(1.0, 0.8, 1), b(50.0, 0.7, 0)];
        let mut out = Vec::new();
        nms(&mut cand, 0.45, &mut out);
        let kept: Vec<(f32, u32)> = out.iter().map(|d| (d.score, d.class)).collect();
        assert_eq!(kept, vec![(0.9, 0), (0.8, 1), (0.7, 0)]);
    }

    #[test]
    fn mask_is_the_positive_side_of_the_prototypes() {
        // 8x8 grid over a 64x64 input (cells of 8 px); one prototype, positive on the left half
        let proto = |_p: usize, _y: usize, x: usize| if x < 4 { 1.0 } else { -1.0 };
        let d = Det { x1: 16.0, y1: 8.0, x2: 48.0, y2: 24.0, score: 1.0, class: 0, anchor: 0 };
        let m = mask(&d, |_| 2.0, 1, proto, 8, 8, 64, 64);
        assert_eq!((m.x, m.y, m.w, m.h, m.cell), (2, 1, 4, 2, 8.0));
        for y in 0..m.h {
            let row: Vec<bool> = (0..m.w).map(|x| m.get(x, y)).collect();
            assert_eq!(row, vec![true, true, false, false]);
        }
    }

    #[test]
    fn top_k_orders_the_classes() {
        let p = [0.1, 0.6, 0.05, 0.25];
        assert_eq!(top_k(|i| p[i], 4, 2), vec![(1, 0.6), (3, 0.25)]);
    }
}

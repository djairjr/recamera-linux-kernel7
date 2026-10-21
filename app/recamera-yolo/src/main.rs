//! recamera-yolo -- run a YOLO model (detect, pose, segment, classify) on the NPU over saved frames
//! (raw RGB packed at the model input size, e.g. the <out>.rgb of
//! recamera-vicap yolo), without the camera. Prints the detections in the
//! .det format of recamera-vicap ("class score x1 y1 x2 y2", model input
//! pixels), so the Rust decoding can be compared with the C one, and the
//! inference time.

use sophgo::yolo::{Hints, Results, Task, Yolo};
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Instant;

const HELP: &str = "\
Usage: recamera-yolo [options] FRAME.rgb...

Run a YOLO model (detect, pose, segment, classify) on saved frames (raw RGB packed, the model
input size: 640x640x3 = 1228800 bytes, as recamera-vicap yolo saves them)
and print the detections, one per line: class score x1 y1 x2 y2 (model
input pixels).

Options:
  -m, --model PATH      model (default
                        /data/models/yolo11n_detection_cv181x_int8.cvimodel)
  -c, --class N|any     target class (default 0, person)
  -t, --threshold T     minimum score, 0..1 (default 0.5)
      --nms T           NMS IoU threshold (default 0.45)
  -n, --repeat N        run each frame N times and print the mean time
      --stats           also print min/max of the first channels of each output
  -h, --help            this help
  -V, --version         version of the program and of the system image
";

fn version() -> String {
    let image = std::fs::read_to_string("/etc/recamera-version")
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "image version unknown".into());
    format!("recamera-yolo {} ({image})", env!("CARGO_PKG_VERSION"))
}

struct Args {
    model: PathBuf,
    class: Option<u32>,
    thr: f32,
    nms: f32,
    repeat: u32,
    stats: bool,
    frames: Vec<PathBuf>,
}

fn parse_args() -> Result<Option<Args>, lexopt::Error> {
    use lexopt::prelude::*;
    let mut a = Args {
        model: PathBuf::from("/data/models/yolo11n_detection_cv181x_int8.cvimodel"),
        class: Some(0),
        thr: 0.5,
        nms: 0.45,
        repeat: 1,
        stats: false,
        frames: Vec::new(),
    };
    let mut p = lexopt::Parser::from_env();
    while let Some(arg) = p.next()? {
        match arg {
            Short('m') | Long("model") => a.model = p.value()?.into(),
            Short('c') | Long("class") => {
                let v = p.value()?.string()?;
                a.class = if v == "any" { None } else { Some(v.parse().map_err(|e| format!("--class: {e}"))?) };
            }
            Short('t') | Long("threshold") => a.thr = p.value()?.parse()?,
            Long("nms") => a.nms = p.value()?.parse()?,
            Short('n') | Long("repeat") => a.repeat = p.value()?.parse::<u32>()?.max(1),
            Long("stats") => a.stats = true,
            Short('h') | Long("help") => {
                print!("{HELP}");
                return Ok(None);
            }
            Short('V') | Long("version") => {
                println!("{}", version());
                return Ok(None);
            }
            Value(v) => a.frames.push(v.into()),
            _ => return Err(arg.unexpected()),
        }
    }
    if a.frames.is_empty() {
        return Err("no frame given".into());
    }
    if !(0.0..1.0).contains(&a.thr) {
        return Err("--threshold: 0..1".into());
    }
    Ok(Some(a))
}

fn main() -> ExitCode {
    let a = match parse_args() {
        Ok(Some(a)) => a,
        Ok(None) => return ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("recamera-yolo: {e}\nTry 'recamera-yolo --help'.");
            return ExitCode::from(2);
        }
    };
    let t0 = Instant::now();
    // what the model's .json says (classes, task, kpt_shape), as recamerad reads it
    let meta = std::fs::read(a.model.with_extension("json")).ok()
        .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
        .unwrap_or_default();
    let kpt: Vec<usize> = meta.get("kpt_shape").and_then(|v| v.as_array())
        .map(|a| a.iter().filter_map(|x| x.as_u64().map(|n| n as usize)).collect()).unwrap_or_default();
    let hints = Hints {
        classes: meta.get("classes").and_then(|c| c.as_array()).map(|a| a.len()),
        task: meta.get("task").and_then(|t| t.as_str()),
        kpt_shape: (kpt.len() == 2).then(|| (kpt[0], kpt[1])),
    };
    let mut yolo = match Yolo::open(&a.model, hints) {
        Ok(y) => y,
        Err(e) => {
            eprintln!("recamera-yolo: {e}");
            return ExitCode::FAILURE;
        }
    };
    let (w, h) = yolo.input_size();
    eprintln!("{}: {}, input {w}x{h} RGB, {} classes, {} keypoints, loaded in {} ms", a.model.display(),
              yolo.task().name(), yolo.classes(), yolo.keypoints(), t0.elapsed().as_millis());
    if let Some(c) = a.class.filter(|&c| c as usize >= yolo.classes()) {
        eprintln!("recamera-yolo: class {c}: the model has {} classes", yolo.classes());
        return ExitCode::from(2);
    }

    let mut ok = true;
    let mut res = Results::default();
    for f in &a.frames {
        let rgb = match std::fs::read(f) {
            Ok(b) => b,
            Err(e) => {
                eprintln!("{}: {e}", f.display());
                ok = false;
                continue;
            }
        };
        let t = Instant::now();
        let mut r = Ok(());
        for _ in 0..a.repeat {
            r = yolo.detect_rgb(&rgb, a.class, a.thr, a.nms, &mut res);
            if r.is_err() {
                break;
            }
        }
        if let Err(e) = r {
            eprintln!("{}: {e}", f.display());
            ok = false;
            continue;
        }
        let ms = t.elapsed().as_secs_f64() * 1000.0 / a.repeat as f64;
        if a.stats {
            for (name, shape, mm) in yolo.output_stats(8) {
                println!("# output {name} {shape:?}");
                for (c, (lo, hi)) in mm.iter().enumerate() {
                    println!("#   channel {c}: {lo} .. {hi}");
                }
            }
        }
        if yolo.task() == Task::Classify {
            println!("# {}: {ms:.1} ms (copy + inference + decoding)", f.display());
            for (c, p) in &res.top {
                println!("{c} {p:.3}");
            }
            continue;
        }
        println!("# {}: {} detection(s), {ms:.1} ms (copy + inference + decoding)", f.display(), res.dets.len());
        for (n, d) in res.dets.iter().enumerate() {
            print!("{} {:.3} {:.1} {:.1} {:.1} {:.1}", d.class, d.score, d.x1, d.y1, d.x2, d.y2);
            // pose: the keypoints after the box, x y visibility each
            for q in res.kpts.iter().skip(n * res.k).take(res.k) {
                print!(" {:.1} {:.1} {:.2}", q[0], q[1], q[2]);
            }
            println!();
        }
    }
    if ok { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}

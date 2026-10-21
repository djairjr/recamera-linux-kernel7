//! recamera-convert -- a YOLOv8/YOLO11 model (.pt or .onnx: detect, pose,
//! segment, classify) to a .cvimodel for the reCamera's cv181x NPU, with the
//! .json recamerad reads (classes, task, keypoints). Runs on a PC.
//!
//! The conversion is Sophgo's TPU-MLIR (and the .pt export Ultralytics),
//! both Python tools: they run in a Docker image this program builds once
//! (docker/Dockerfile). This program checks the model, drives the steps,
//! checks that the result has an output layout the camera decodes, and
//! writes the .json.
//!
//! The decoding tail of the network (after the last convolutions of the
//! head) is kept in BF16 and the rest is INT8: in INT8 the final concat
//! shares one scale between box coordinates (0..640) and scores (0..1) and
//! the scores come out zero.

use lexopt::prelude::*;
use recamera_detect::decode::{self, Layout};
use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};

const IMAGE: &str = "recamera-convert:1";
const DOCKERFILE: &str = include_str!("../docker/Dockerfile");
const HELPER: &str = include_str!("../docker/convert.py");

const HELP: &str = "\
Usage: recamera-convert <model.pt|model.onnx> --images DIR [options]

Convert a YOLOv8/YOLO11 model (detect, pose, segment or classify) to a
.cvimodel for the reCamera NPU, and write the .json recamerad reads next to
it. Upload both on the camera's YOLO page.

Options:
  -i, --images DIR     calibration pictures (.jpg/.png), like the scenes the
                       camera will see; 100 or more is good (required)
  -o, --out DIR        where to write <name>.cvimodel and <name>.json (default .)
  -n, --name NAME      model name (default: the input file name)
      --count N        pictures used for calibration (default 100)
      --imgsz N        export size of a .pt (default 640)
      --extra FILE     a JSON object merged into the .json (e.g. \"skeleton\",
                       \"kpt_names\" for a pose model)
      --keep           keep the working files (<out>/.<name>-work)
  -h, --help           this help
  -V, --version        version

Needs Docker. The first run builds the image recamera-convert:1 (Sophgo
tpuc_dev + tpu_mlir + Ultralytics, a few GB); a conversion takes a few
minutes on the CPU.
";

struct Args {
    model: PathBuf,
    images: PathBuf,
    out: PathBuf,
    name: Option<String>,
    count: u32,
    imgsz: u32,
    extra: Option<PathBuf>,
    keep: bool,
}

fn parse_args() -> Result<Option<Args>, String> {
    let mut p = lexopt::Parser::from_env();
    let (mut model, mut images) = (None, None);
    let mut a = Args {
        model: PathBuf::new(),
        images: PathBuf::new(),
        out: PathBuf::from("."),
        name: None,
        count: 100,
        imgsz: 640,
        extra: None,
        keep: false,
    };
    let num = |v: std::ffi::OsString| -> Result<u32, String> {
        v.to_string_lossy().parse::<u32>().map_err(|e| e.to_string())
    };
    while let Some(arg) = p.next().map_err(|e| e.to_string())? {
        let val = |p: &mut lexopt::Parser| p.value().map_err(|e| e.to_string());
        match arg {
            Short('i') | Long("images") => images = Some(PathBuf::from(val(&mut p)?)),
            Short('o') | Long("out") => a.out = PathBuf::from(val(&mut p)?),
            Short('n') | Long("name") => a.name = Some(val(&mut p)?.to_string_lossy().into_owned()),
            Long("count") => a.count = num(val(&mut p)?)?.max(1),
            Long("imgsz") => a.imgsz = num(val(&mut p)?)?,
            Long("extra") => a.extra = Some(PathBuf::from(val(&mut p)?)),
            Long("keep") => a.keep = true,
            Short('h') | Long("help") => {
                print!("{HELP}");
                return Ok(None);
            }
            Short('V') | Long("version") => {
                println!("recamera-convert {}", env!("CARGO_PKG_VERSION"));
                return Ok(None);
            }
            Value(v) if model.is_none() => model = Some(PathBuf::from(v)),
            _ => return Err(arg.unexpected().to_string()),
        }
    }
    a.model = model.ok_or("no model given")?;
    a.images = images.ok_or("--images DIR is required (calibration pictures)")?;
    Ok(Some(a))
}

fn main() -> ExitCode {
    let a = match parse_args() {
        Ok(Some(a)) => a,
        Ok(None) => return ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("recamera-convert: {e}\nTry 'recamera-convert --help'.");
            return ExitCode::from(2);
        }
    };
    match run(&a) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("recamera-convert: {e}");
            ExitCode::FAILURE
        }
    }
}

fn say(s: &str) {
    eprintln!("== {s}");
}

fn run(a: &Args) -> Result<(), String> {
    let ext = a.model.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
    if ext != "pt" && ext != "onnx" {
        return Err(format!("{}: a .pt or .onnx model", a.model.display()));
    }
    let images = fs::canonicalize(&a.images).map_err(|e| format!("{}: {e}", a.images.display()))?;
    let stem = a.model.file_stem().and_then(|s| s.to_str()).unwrap_or("model").to_string();
    let name: String = a.name.clone().unwrap_or(stem)
        .chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' }).collect();
    fs::create_dir_all(&a.out).map_err(|e| format!("{}: {e}", a.out.display()))?;
    let out = fs::canonicalize(&a.out).map_err(|e| e.to_string())?;
    let work = out.join(format!(".{name}-work"));
    let _ = fs::remove_dir_all(&work);
    fs::create_dir_all(&work).map_err(|e| format!("{}: {e}", work.display()))?;
    fs::write(work.join("convert.py"), HELPER).map_err(|e| e.to_string())?;

    ensure_image()?;
    let docker = Docker { work: work.clone(), images };

    // the model into the working directory (a .pt that does not exist is an
    // Ultralytics name, e.g. yolo11n-pose.pt: the export downloads it)
    let file = a.model.file_name().ok_or("bad model path")?.to_string_lossy().into_owned();
    if a.model.exists() {
        fs::copy(&a.model, work.join(&file)).map_err(|e| format!("{}: {e}", a.model.display()))?;
    } else if ext != "pt" {
        return Err(format!("{}: no such file", a.model.display()));
    }
    let onnx = if ext == "pt" {
        say(&format!("exporting {file} to ONNX ({}x{})", a.imgsz, a.imgsz));
        let r = docker.helper(&["export", &file, &a.imgsz.to_string()])?;
        r["onnx"].as_str().ok_or("export: no ONNX")?.rsplit('/').next().unwrap_or_default().to_string()
    } else {
        file.clone()
    };

    say(&format!("inspecting {onnx}"));
    let info = docker.helper(&["inspect", &onnx])?;
    let shape = |v: &Value| -> Vec<i32> {
        v["shape"].as_array().map(|a| a.iter().map(|d| d.as_i64().unwrap_or(-1) as i32).collect()).unwrap_or_default()
    };
    let input = info["inputs"].as_array().and_then(|i| i.first()).map(shape).unwrap_or_default();
    if input.len() != 4 || input[1] != 3 || input.iter().any(|&d| d <= 0) {
        return Err(format!("input shape {input:?}: a fixed [1, 3, H, W] RGB input is needed (export without dynamic shapes)"));
    }
    let outputs: Vec<Vec<i32>> = info["outputs"].as_array().map(|o| o.iter().map(shape).collect()).unwrap_or_default();
    let names: Vec<String> = info["names"].as_array()
        .map(|n| n.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
    let task = info["task"].as_str().map(String::from);
    let kpt: Option<(usize, usize)> = info["kpt_shape"].as_array()
        .filter(|k| k.len() == 2)
        .and_then(|k| Some((k[0].as_u64()? as usize, k[1].as_u64()? as usize)));
    if let Some(t) = &task
        && !["detect", "pose", "segment", "classify"].contains(&t.as_str())
    {
        return Err(format!("task {t}: the camera runs detect, pose, segment and classify"));
    }

    // the layout the camera will see (the cvimodel keeps the ONNX shapes)
    let nc = (!names.is_empty()).then_some(names.len());
    let layout = decode::layout(&outputs, nc, task.as_deref(), kpt)
        .map_err(|e| format!("outputs {outputs:?}: {e}; the camera cannot decode this model"))?;
    let (found_task, classes, keypoints) = match layout {
        Layout::Split => ("detect", nc.unwrap_or(0), 0),
        Layout::Flat { nc, extra, .. } => match extra {
            decode::Extra::None => ("detect", nc, 0),
            decode::Extra::Pose { k, .. } => ("pose", nc, k),
            decode::Extra::Segment { .. } => ("segment", nc, 0),
        },
        Layout::Classify { nc, .. } => ("classify", nc, 0),
    };
    if let Some(t) = &task
        && t != found_task
    {
        return Err(format!("the model says {t}, its outputs are those of {found_task}"));
    }
    if !names.is_empty() && names.len() != classes {
        return Err(format!("{} class names for {classes} classes", names.len()));
    }
    say(&format!("{found_task}, {classes} classes{}, input {}x{}, outputs {outputs:?}",
                 if keypoints > 0 { format!(", {keypoints} keypoints") } else { String::new() }, input[3], input[2]));

    say(&format!("converting: TPU-MLIR, INT8 calibrated on {} pictures, BF16 decoding tail (a few minutes)", a.count));
    let r = docker.helper(&["convert", &format!("/w/{onnx}"), &name, "/images", &a.count.to_string(), "/w"])?;
    let similarity = fs::read_to_string(work.join("tpu-mlir.log")).ok()
        .and_then(|l| l.lines().rev().find(|l| l.contains("min_similarity")).map(|l| l.trim().to_string()));

    let cvimodel = out.join(format!("{name}.cvimodel"));
    fs::copy(work.join(format!("{name}.cvimodel")), &cvimodel).map_err(|e| format!("cvimodel: {e}"))?;
    let mut meta = json!({
        "model_name": name,
        "model_format": "cvimodel",
        "task": found_task,
        "classes": if names.is_empty() { (0..classes).map(|i| format!("class {i}")).collect() } else { names },
        "description": format!("{} -- converted by recamera-convert {} from {file}: INT8, BF16 decoding tail ({} ops), \
                                calibrated on {} pictures",
                               info["description"].as_str().unwrap_or("YOLO model"), env!("CARGO_PKG_VERSION"),
                               r["bf16_ops"], a.count),
    });
    if let (Some((k, d)), Some(o)) = (kpt.or((keypoints > 0).then_some((keypoints, 3))), meta.as_object_mut()) {
        o.insert("kpt_shape".into(), json!([k, d]));
    }
    if let Some(x) = &a.extra {
        let extra: Value = fs::read(x).map_err(|e| format!("{}: {e}", x.display()))
            .and_then(|b| serde_json::from_slice(&b).map_err(|e| format!("{}: {e}", x.display())))?;
        let (Some(o), Some(e)) = (meta.as_object_mut(), extra.as_object()) else {
            return Err(format!("{}: not a JSON object", x.display()));
        };
        for (k, v) in e {
            o.insert(k.clone(), v.clone());
        }
    }
    let json_path = out.join(format!("{name}.json"));
    fs::write(&json_path, serde_json::to_string_pretty(&meta).map_err(|e| e.to_string())? + "\n")
        .map_err(|e| format!("{}: {e}", json_path.display()))?;

    if !a.keep {
        let _ = fs::remove_dir_all(&work);
    }
    say(&format!("done: {} and {}", cvimodel.display(), json_path.display()));
    if let Some(s) = similarity {
        say(&format!("simulated INT8 model against the float ONNX on one picture: {s}"));
    }
    eprintln!("Upload both files on the camera's YOLO page, then Use.");
    Ok(())
}

/// Build the image the first time.
fn ensure_image() -> Result<(), String> {
    let have = Command::new("docker").args(["image", "inspect", IMAGE]).stdout(Stdio::null()).stderr(Stdio::null())
        .status().map_err(|e| format!("docker: {e} (is Docker installed?)"))?;
    if have.success() {
        return Ok(());
    }
    say(&format!("building the image {IMAGE} (once; it downloads a few GB)"));
    let dir = std::env::temp_dir().join(format!("recamera-convert-image-{}", std::process::id()));
    fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    fs::write(dir.join("Dockerfile"), DOCKERFILE).map_err(|e| e.to_string())?;
    let st = Command::new("docker").args(["build", "-t", IMAGE]).arg(&dir).status().map_err(|e| e.to_string())?;
    let _ = fs::remove_dir_all(&dir);
    if st.success() { Ok(()) } else { Err(format!("docker build of {IMAGE} failed")) }
}

struct Docker {
    work: PathBuf,
    images: PathBuf,
}

impl Docker {
    /// Run a step of convert.py; its last stdout line is the JSON result.
    fn helper(&self, args: &[&str]) -> Result<Value, String> {
        let uid = |flag: &str| -> String {
            Command::new("id").arg(flag).output().ok()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_else(|| "0".into())
        };
        let mount = |p: &Path, at: &str, ro: bool| format!("{}:{at}{}", p.display(), if ro { ":ro" } else { "" });
        let o = Command::new("docker")
            .args(["run", "--rm", "-u", &format!("{}:{}", uid("-u"), uid("-g")), "-w", "/w"])
            .args(["-v", &mount(&self.work, "/w", false), "-v", &mount(&self.images, "/images", true)])
            .args([IMAGE, "python3", "/w/convert.py"])
            .args(args)
            .stderr(Stdio::inherit())
            .output()
            .map_err(|e| format!("docker: {e}"))?;
        let stdout = String::from_utf8_lossy(&o.stdout);
        if !o.status.success() {
            return Err(format!("{} failed: {}", args[0], stdout.lines().last().unwrap_or("")));
        }
        let last = stdout.lines().rev().find(|l| l.starts_with('{')).ok_or(format!("{}: no result", args[0]))?;
        serde_json::from_str(last).map_err(|e| format!("{}: {e}", args[0]))
    }
}

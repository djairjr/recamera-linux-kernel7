//! The configuration: one JSON file, /data/recamera/config.json by default
//! (docs/application.md: single config.json in /data/recamera,
//! atomic writes). Every field has a default, so an older or partial file
//! still loads; a missing file is created with the defaults; an invalid file
//! is never overwritten (the user's edit stays there to be fixed).

use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

pub const DEFAULT_PATH: &str = "/data/recamera/config.json";

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub camera: Camera,
    pub detection: Detection,
    pub stream: Stream,
    pub led: Led,
    pub web: Web,
    pub rtsp: Rtsp,
    pub system: System,
    pub mqtt: Mqtt,
    pub llm: Llm,
    pub telemetry: Telemetry,
    pub events: Events,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Camera {
    /// Sensor mode: "1280x960" (default, F7 decision), "1920x1080" or "640x480".
    pub sensor_mode: String,
    /// picture adjustments of the ISP, 0..100, 50 = as tuned
    pub brightness: u32,
    pub contrast: u32,
    pub saturation: u32,
    pub hue: u32,
    pub sharpness: u32,
    /// left-right and upside-down (a camera mounted the other way)
    pub mirror: bool,
    pub flip: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Detection {
    pub enabled: bool,
    /// .cvimodel in /data/models
    pub model: String,
    /// target class index in the model's labels (COCO: 0 person, 16 dog)
    pub target_class: u32,
    /// minimum score, 0..1
    pub threshold: f32,
    /// IoU above which two boxes of a class are the same object, 0..1
    pub nms: f32,
    /// minimum box area, fraction of the stream image (0: any)
    pub min_area: f32,
    /// area of interest: the target counts when the bottom centre of its box
    /// (its base) is inside; a rectangle relative to the stream image;
    /// null = the whole image
    pub roi: Option<recamera_detect::Roi>,
    /// consecutive frames with the target before "arrived"
    pub confirm_frames: u32,
    /// seconds without the target before "left"
    pub leave_after_s: f32,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Stream {
    /// MJPEG frames per second sent to the browsers (the encoder only runs
    /// while someone watches)
    pub mjpeg_fps: u32,
    /// JPEG quality 1..99 (MJPEG and snapshots)
    pub jpeg_quality: u32,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Led {
    /// white LED brightness, 0..255 (manual; automatic by the ISP comes in A12)
    pub brightness: u32,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Web {
    /// HTTP port of the page and the streams (0: off)
    pub port: u16,
    /// the Terminal page (a shell for the logged-in account)
    pub terminal: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Rtsp {
    /// RTSP port (0: off); the stream is rtsp://<camera>/live
    pub port: u16,
    /// H.264 bit rate, kbit/s (CBR)
    pub kbps: u32,
    /// frames between IDRs
    pub gop: u32,
    /// ask for the system user and password (Basic), as the page does
    pub auth: bool,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct System {
    /// time zone, a name of /usr/share/zoneinfo ("America/Sao_Paulo")
    pub timezone: String,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Mqtt {
    pub enabled: bool,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: String,
    /// TLS to the broker (port 8883 usually), certificates of the system
    pub tls: bool,
    /// topics under "<base>/<device id>"
    pub base_topic: String,
    /// Home Assistant MQTT discovery
    pub discovery: bool,
    pub discovery_prefix: String,
    /// the device name shown in Home Assistant
    pub device_name: String,
    /// seconds between the frames of the Home Assistant camera entity (0: off)
    pub camera_interval_s: u32,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Llm {
    /// Send the frame of every arrival to an LLM/VLM (switch in the UI).
    pub enabled: bool,
    /// https://.../v1/chat/completions (the OpenAI API, which OpenAI, Ollama,
    /// OpenRouter, vLLM and others speak) or ws(s)://... (one JSON message
    /// out, one answer back; see llm.rs)
    pub endpoint: String,
    pub model: String,
    /// sent as "Authorization: Bearer <key>" (empty: no header)
    pub api_key: String,
    /// the question; {label} becomes the target's name
    pub prompt: String,
    pub max_tokens: u32,
    pub timeout_s: u32,
    /// at most one request in this many seconds (arrivals in between are
    /// not sent)
    pub min_interval_s: u32,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Telemetry {
    /// seconds between telemetry events (0: off)
    pub interval_s: u64,
}

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Events {
    /// Unix socket streaming the events as JSON lines
    pub socket: String,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            camera: Camera::default(),
            detection: Detection::default(),
            stream: Stream::default(),
            led: Led::default(),
            web: Web::default(),
            rtsp: Rtsp::default(),
            system: System::default(),
            mqtt: Mqtt::default(),
            llm: Llm::default(),
            telemetry: Telemetry::default(),
            events: Events::default(),
        }
    }
}

impl Default for Camera {
    fn default() -> Self {
        Camera {
            sensor_mode: "1280x960".into(),
            brightness: 50,
            contrast: 50,
            saturation: 50,
            hue: 50,
            sharpness: 50,
            mirror: false,
            flip: false,
        }
    }
}

impl Default for Detection {
    fn default() -> Self {
        Detection {
            enabled: true,
            model: "yolo11n_detection_cv181x_int8.cvimodel".into(),
            target_class: 0,
            threshold: 0.5,
            nms: 0.45,
            min_area: 0.0,
            roi: None,
            confirm_frames: 3,
            leave_after_s: 2.0,
        }
    }
}

impl Default for Stream {
    fn default() -> Self {
        Stream { mjpeg_fps: 15, jpeg_quality: 80 }
    }
}

impl Default for Led {
    fn default() -> Self {
        Led { brightness: 0 }
    }
}

impl Default for Web {
    fn default() -> Self {
        Web { port: 80, terminal: true }
    }
}

impl Default for Rtsp {
    fn default() -> Self {
        Rtsp { port: 554, kbps: 2048, gop: 30, auth: true }
    }
}

impl Default for System {
    fn default() -> Self {
        System { timezone: "UTC".into() }
    }
}

impl Default for Mqtt {
    fn default() -> Self {
        Mqtt {
            enabled: false,
            host: String::new(),
            port: 1883,
            username: String::new(),
            password: String::new(),
            tls: false,
            base_topic: "recamera".into(),
            discovery: true,
            discovery_prefix: "homeassistant".into(),
            device_name: "reCamera".into(),
            camera_interval_s: 5,
        }
    }
}

impl Default for Llm {
    fn default() -> Self {
        Llm {
            enabled: false,
            endpoint: String::new(),
            model: String::new(),
            api_key: String::new(),
            prompt: "A {label} was just detected by a camera. Describe in one short sentence what it is doing."
                .into(),
            max_tokens: 200,
            timeout_s: 30,
            min_interval_s: 60,
        }
    }
}

impl Default for Telemetry {
    fn default() -> Self {
        Telemetry { interval_s: 10 }
    }
}

impl Default for Events {
    fn default() -> Self {
        Events { socket: "/run/recamerad/events.sock".into() }
    }
}

impl Config {
    /// Range checks the types cannot express.
    pub fn validate(&self) -> Result<(), String> {
        if !matches!(self.camera.sensor_mode.as_str(), "1280x960" | "1920x1080" | "640x480") {
            return Err(format!("camera.sensor_mode {:?}: 1280x960, 1920x1080 or 640x480",
                               self.camera.sensor_mode));
        }
        if self.mqtt.enabled && self.mqtt.host.trim().is_empty() {
            return Err("mqtt.host: the broker address".into());
        }
        let topic_ok = |t: &str| !t.is_empty() && !t.contains(['+', '#']) && !t.starts_with('/') && !t.ends_with('/');
        let e = self.llm.endpoint.trim();
        if (self.llm.enabled || !e.is_empty())
            && !["http://", "https://", "ws://", "wss://"].iter().any(|p| e.starts_with(p))
        {
            return Err("llm.endpoint: an http(s):// or ws(s):// URL".into());
        }
        if !(1..=300).contains(&self.llm.timeout_s) {
            return Err(format!("llm.timeout_s {}: 1..300", self.llm.timeout_s));
        }
        if !topic_ok(&self.mqtt.base_topic) || !topic_ok(&self.mqtt.discovery_prefix) {
            return Err("mqtt topics: not empty, no + # or leading/trailing /".into());
        }
        let tz = &self.system.timezone;
        if tz.is_empty() || tz.starts_with('/') || tz.contains("..") {
            return Err(format!("system.timezone {tz:?}: a zone name such as America/Sao_Paulo"));
        }
        let c = &self.camera;
        for (name, v) in [("brightness", c.brightness), ("contrast", c.contrast), ("saturation", c.saturation),
                          ("hue", c.hue), ("sharpness", c.sharpness)] {
            if v > 100 {
                return Err(format!("camera.{name} {v}: 0..100"));
            }
        }
        if !(0.0..=1.0).contains(&self.detection.threshold) {
            return Err(format!("detection.threshold {}: 0..1", self.detection.threshold));
        }
        if self.detection.confirm_frames == 0 {
            return Err("detection.confirm_frames: at least 1".into());
        }
        if !(0.0..=1.0).contains(&self.detection.nms) {
            return Err(format!("detection.nms {}: 0..1", self.detection.nms));
        }
        if !(0.0..=1.0).contains(&self.detection.min_area) {
            return Err(format!("detection.min_area {}: 0..1", self.detection.min_area));
        }
        if !(0.0..=600.0).contains(&self.detection.leave_after_s) {
            return Err(format!("detection.leave_after_s {}: 0..600", self.detection.leave_after_s));
        }
        if !(1..=30).contains(&self.stream.mjpeg_fps) {
            return Err(format!("stream.mjpeg_fps {}: 1..30", self.stream.mjpeg_fps));
        }
        if !(1..=99).contains(&self.stream.jpeg_quality) {
            return Err(format!("stream.jpeg_quality {}: 1..99", self.stream.jpeg_quality));
        }
        if !(256..=8192).contains(&self.rtsp.kbps) {
            return Err(format!("rtsp.kbps {}: 256..8192", self.rtsp.kbps));
        }
        if !(1..=300).contains(&self.rtsp.gop) {
            return Err(format!("rtsp.gop {}: 1..300", self.rtsp.gop));
        }
        if self.led.brightness > 255 {
            return Err(format!("led.brightness {}: 0..255", self.led.brightness));
        }
        if let Some(r) = &self.detection.roi {
            r.validate().map_err(|e| format!("detection.{e}"))?;
        }
        Ok(())
    }
}

pub enum Loaded {
    /// read and valid
    File(Config),
    /// no file: defaults, and the file was created
    Created(Config),
}

/// Read the file; create it with the defaults when it does not exist.
pub fn load(path: &Path) -> Result<Loaded, String> {
    match fs::read(path) {
        Ok(bytes) => {
            let c: Config = serde_json::from_slice(&bytes)
                .map_err(|e| format!("{}: {e}", path.display()))?;
            c.validate().map_err(|e| format!("{}: {e}", path.display()))?;
            Ok(Loaded::File(c))
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            let c = Config::default();
            save(path, &c).map_err(|e| format!("{}: cannot create: {e}", path.display()))?;
            Ok(Loaded::Created(c))
        }
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// Atomic write: temporary file in the same directory, fsync, rename over the
/// old one, fsync the directory. A power cut leaves the old or the new file,
/// never a truncated one.
pub fn save(path: &Path, c: &Config) -> io::Result<()> {
    let dir = path.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
    fs::create_dir_all(dir)?;
    let mut tmp = PathBuf::from(path);
    tmp.set_extension("json.tmp");
    let mut json = serde_json::to_vec_pretty(c).map_err(io::Error::other)?;
    json.push(b'\n');
    {
        let mut f = File::create(&tmp)?;
        f.write_all(&json)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    File::open(dir)?.sync_all()
}

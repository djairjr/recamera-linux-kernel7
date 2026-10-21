//! recamerad -- the reCamera daemon (docs/application.md): configuration,
//! logging, signals, JSON events and telemetry (A1); the media pipeline,
//! camera -> VPSS with the YOLO and stream channels (A2), YOLO on the NPU
//! (A3), target arrival/departure events (A4), the web page with the MJPEG
//! stream, the boxes and the configuration (A5), H.264 over RTSP (A6) and
//! WebRTC/WHEP for the page (A7), the system pages (A8: models, network,
//! MQTT, the LLM and the terminal).

mod config;
mod events;
mod live;
mod llm;
mod log;
mod media;
mod mqtt;
mod network;
mod rtsp;
mod system;
mod telemetry;
mod terminal;
mod update;
mod web;
#[cfg(feature = "webrtc")]
mod webrtc;

use config::{Config, Loaded};
use events::Bus;
use live::Live;
use media::Media;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

const HELP: &str = "\
Usage: recamerad [options]

The reCamera daemon: camera, detection, streaming and configuration.

Options:
  -c, --config PATH   configuration file (default /data/recamera/config.json;
                      created with the defaults when missing)
  -f, --foreground    log to stderr instead of syslog
      --events        also write the events (JSON lines) to stdout
      --no-media      do not open the camera (configuration and events only)
  -v, --verbose       debug messages
      --check         validate the configuration file and exit (0 = valid)
  -h, --help          this help
  -V, --version       version of the program and of the system image

Signals: SIGTERM/SIGINT stop, SIGHUP reloads the configuration.
Events: JSON lines on the Unix socket of the configuration (events.socket,
default /run/recamerad/events.sock): recamera-events prints them.
";

struct Args {
    config: PathBuf,
    foreground: bool,
    events: bool,
    verbose: bool,
    check: bool,
    no_media: bool,
}

fn version() -> String {
    let image = std::fs::read_to_string("/etc/recamera-version")
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "image version unknown".into());
    format!("recamerad {} ({image})", env!("CARGO_PKG_VERSION"))
}

fn parse_args() -> Result<Option<Args>, lexopt::Error> {
    use lexopt::prelude::*;
    let mut a = Args {
        config: PathBuf::from(config::DEFAULT_PATH),
        foreground: false,
        events: false,
        verbose: false,
        check: false,
        no_media: false,
    };
    let mut p = lexopt::Parser::from_env();
    while let Some(arg) = p.next()? {
        match arg {
            Short('c') | Long("config") => a.config = p.value()?.into(),
            Short('f') | Long("foreground") => a.foreground = true,
            Long("events") => a.events = true,
            Short('v') | Long("verbose") => a.verbose = true,
            Long("check") => a.check = true,
            Long("no-media") => a.no_media = true,
            Short('h') | Long("help") => {
                print!("{HELP}");
                return Ok(None);
            }
            Short('V') | Long("version") => {
                println!("{}", version());
                return Ok(None);
            }
            _ => return Err(arg.unexpected()),
        }
    }
    Ok(Some(a))
}

/// The tasks that depend on the configuration, restarted on a reload.
struct Tasks {
    handles: Vec<JoinHandle<()>>,
}

impl Tasks {
    fn start(bus: &Bus, c: &Config) -> Self {
        let mut handles = vec![tokio::spawn(events::serve_socket(bus.clone(), c.events.socket.clone()))];
        if c.telemetry.interval_s > 0 {
            handles.push(tokio::spawn(telemetry::run(bus.clone(), Duration::from_secs(c.telemetry.interval_s))));
        }
        Tasks { handles }
    }

    fn stop(self) {
        for h in self.handles {
            h.abort();
        }
    }
}

/// The white LED (pwm-leds, F7): manual brightness from the configuration.
fn apply_led(brightness: u32) {
    if let Err(e) = std::fs::write("/sys/class/leds/white/brightness", brightness.min(255).to_string()) {
        warn!("led: {e}");
    }
}

/// Stop the pipeline without blocking the network thread; a teardown stuck
/// in the MPI is left behind (the next start reclaims what it holds).
async fn stop_media(m: Option<Media>) {
    let Some(m) = m else { return };
    let h = tokio::task::spawn_blocking(move || m.stop());
    if tokio::time::timeout(Duration::from_secs(5), h).await.is_err() {
        warn!("media: teardown did not finish in 5 s");
    }
}

async fn run(args: Args) -> ExitCode {
    let bus = Bus::new();
    if args.events {
        tokio::spawn(events::to_stdout(bus.clone()));
    }

    let (mut cfg, created) = match config::load(&args.config) {
        Ok(Loaded::File(c)) => (c, false),
        Ok(Loaded::Created(c)) => (c, true),
        Err(e) => {
            // never overwrite the user's file: run on the defaults and say so
            error!("configuration: {e}; running with the defaults");
            bus.emit("config_error", &serde_json::json!({ "error": e }));
            (Config::default(), false)
        }
    };
    if created {
        info!("created {} with the defaults", args.config.display());
    }

    let mut tasks = Tasks::start(&bus, &cfg);
    let live = Live::new(cfg.stream.mjpeg_fps);
    let start_media = |c: &Config| (!args.no_media).then(|| Media::start(c, bus.clone(), live.clone()));
    let mut media = start_media(&cfg);
    apply_led(cfg.led.brightness);
    if let Err(e) = system::apply_timezone(&cfg.system.timezone) {
        warn!("{e}");
    }

    // the page: reads the running configuration, asks for a reload after
    // saving one
    let (cfg_tx, cfg_rx) = watch::channel(cfg.clone());
    let (reload_tx, mut reload_rx) = mpsc::channel::<()>(4);
    let reload_tx_mqtt = reload_tx.clone();
    let web = web::Web::new(live.clone(), bus.clone(), args.config.clone(), cfg_rx, reload_tx, version());
    let start_web = |c: &Config| (c.web.port > 0).then(|| tokio::spawn(web::serve(web.clone(), c.web.port)));
    let mut web_task = start_web(&cfg);
    let start_rtsp = |c: &Config| {
        (c.rtsp.port > 0).then(|| tokio::spawn(rtsp::serve(live.clone(), c.rtsp.port, c.rtsp.auth)))
    };
    let mut rtsp_task = start_rtsp(&cfg);
    let ctrl = mqtt::Control { config_path: args.config.clone(), config: cfg_tx.subscribe(), reload: reload_tx_mqtt };
    let start_mqtt = |c: &Config| {
        c.mqtt.enabled.then(|| {
            tokio::spawn(mqtt::run(c.mqtt.clone(), ctrl.clone(), bus.clone(), live.clone(), version()))
        })
    };
    let mut mqtt_task = start_mqtt(&cfg);
    let start_llm = |c: &Config| {
        (c.llm.enabled && !args.no_media).then(|| tokio::spawn(llm::run(c.llm.clone(), bus.clone(), live.clone())))
    };
    let mut llm_task = start_llm(&cfg);

    // let the socket bind before the first event
    tokio::task::yield_now().await;
    info!("{} started", version());
    bus.emit("started", &serde_json::json!({
        "version": version(),
        "config": args.config.display().to_string(),
        "config_created": created,
    }));

    let (Ok(mut term), Ok(mut int), Ok(mut hup)) = (
        signal(SignalKind::terminate()),
        signal(SignalKind::interrupt()),
        signal(SignalKind::hangup()),
    ) else {
        error!("cannot install the signal handlers");
        return ExitCode::FAILURE;
    };

    let why = loop {
        let from = tokio::select! {
            _ = term.recv() => break "SIGTERM",
            _ = int.recv() => break "SIGINT",
            _ = hup.recv() => "SIGHUP",
            Some(()) = reload_rx.recv() => "web",
        };
        match config::load(&args.config) {
            Ok(Loaded::File(c)) | Ok(Loaded::Created(c)) => {
                if c == cfg {
                    info!("configuration unchanged ({from})");
                    continue;
                }
                tasks.stop();
                tasks = Tasks::start(&bus, &c);
                if c.camera.sensor_mode != cfg.camera.sensor_mode
                    || c.stream.jpeg_quality != cfg.stream.jpeg_quality
                    || (c.rtsp.kbps, c.rtsp.gop) != (cfg.rtsp.kbps, cfg.rtsp.gop)
                    || Media::needs_restart(&cfg.detection, &c.detection)
                {
                    stop_media(media.take()).await;
                    media = start_media(&c);
                } else if let Some(m) = &media {
                    m.update_detection(&c.detection);
                    m.update_camera(&c.camera);
                }
                live.mjpeg_fps.store(c.stream.mjpeg_fps, Ordering::Relaxed);
                if c.led != cfg.led {
                    apply_led(c.led.brightness);
                }
                if c.system != cfg.system && let Err(e) = system::apply_timezone(&c.system.timezone) {
                    warn!("{e}");
                }
                if (c.rtsp.port, c.rtsp.auth) != (cfg.rtsp.port, cfg.rtsp.auth) {
                    if let Some(t) = rtsp_task.take() {
                        t.abort();
                    }
                    rtsp_task = start_rtsp(&c);
                }
                if c.mqtt != cfg.mqtt {
                    if let Some(t) = mqtt_task.take() {
                        t.abort();
                    }
                    mqtt_task = start_mqtt(&c);
                }
                if c.llm != cfg.llm {
                    if let Some(t) = llm_task.take() {
                        t.abort();
                    }
                    llm_task = start_llm(&c);
                }
                // web.terminal is read at every request: no restart
                if c.web.port != cfg.web.port {
                    if let Some(t) = web_task.take() {
                        t.abort();
                    }
                    web_task = start_web(&c);
                }
                cfg = c;
                cfg_tx.send_replace(cfg.clone());
                info!("configuration reloaded ({from})");
                bus.emit("config_reloaded", &serde_json::json!({ "from": from }));
            }
            Err(e) => {
                error!("reload: {e}; keeping the running configuration");
                bus.emit("config_error", &serde_json::json!({ "error": e }));
            }
        }
    };

    info!("stopping ({why})");
    bus.emit("stopping", &serde_json::json!({ "signal": why }));
    for t in [web_task.take(), rtsp_task.take(), mqtt_task.take(), llm_task.take()].into_iter().flatten() {
        t.abort();
    }
    stop_media(media.take()).await;
    // let the event sinks flush the last line
    tokio::time::sleep(Duration::from_millis(50)).await;
    tasks.stop();
    let _ = std::fs::remove_file(&cfg.events.socket);
    ExitCode::SUCCESS
}

fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(Some(a)) => a,
        Ok(None) => return ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("recamerad: {e}\nTry 'recamerad --help'.");
            return ExitCode::from(2);
        }
    };

    if args.check {
        return match std::fs::read(&args.config)
            .map_err(|e| e.to_string())
            .and_then(|b| serde_json::from_slice::<Config>(&b).map_err(|e| e.to_string()))
            .and_then(|c| c.validate())
        {
            Ok(()) => {
                println!("{}: valid", args.config.display());
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("{}: {e}", args.config.display());
                ExitCode::FAILURE
            }
        };
    }

    log::init(args.foreground, args.verbose);
    let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => {
            error!("tokio runtime: {e}");
            return ExitCode::FAILURE;
        }
    };
    rt.block_on(run(args))
}

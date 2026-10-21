//! The web page, its JSON API and the streams (stage A5), HTTP/1 on the
//! network thread (docs/application.md §1).
//!
//! - `/`, `/app.js`, `/app.css`: the page, built into the binary (web/)
//! - `POST /api/login`: the system account (/etc/shadow), as the Seeed page
//!   did; a session cookie (HttpOnly) for everything else
//! - `GET|POST /api/config`, `GET /api/status`, `GET /api/labels`,
//!   `GET /api/model` (task, classes, keypoints and skeleton of the running model)
//! - `/stream.mjpg` (multipart JPEG), `/snapshot.jpg`
//! - `/live`: Server-Sent Events, the boxes of every frame ("dets") and the
//!   daemon events
//! - `POST /whep`, `DELETE /whep/<id>`: WebRTC (feature "webrtc", stage A7)
//! - `GET /api/llm`, `POST /api/llm/test`: the last answers, a try with the
//!   current frame (A8b)
//! - `GET /api/terminal`: the terminal's WebSocket (A8c, terminal.rs)
//! - `GET /api/update`, `POST /api/update` (the .rcu as the body, streamed
//!   into the inactive slot), `POST /api/update/reboot` (A8d, update.rs)
//! - `/vendor/…`: xterm.js for the terminal (web/vendor, MIT)
//!
//! Every stream is a task that reads the newest value (tokio watch) and
//! writes into a 2-slot channel the response body reads from: a slow browser
//! only skips frames, and nothing is kept per client but that channel.

use crate::config::{self, Config};
use crate::events::Bus;
use crate::live::Live;
use crate::{media, network, system, telemetry};
use bytes::Bytes;
use http_body_util::combinators::BoxBody;
use http_body_util::{BodyExt, Full, Limited};
use hyper::body::{Body, Frame, Incoming};
use hyper::header::{self, HeaderValue};
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use std::collections::HashMap;
use std::convert::Infallible;
use std::ffi::{CStr, CString, c_char};
use std::net::IpAddr;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};
use tokio::net::TcpListener;
use tokio::sync::{broadcast, mpsc, watch};

type RespBody = BoxBody<Bytes, Infallible>;

const INDEX_HTML: &str = include_str!("../web/index.html");
const APP_JS: &str = include_str!("../web/app.js");
const APP_CSS: &str = include_str!("../web/app.css");
const XTERM_JS: &str = include_str!("../web/vendor/xterm.js");
const XTERM_CSS: &str = include_str!("../web/vendor/xterm.css");
const XTERM_FIT_JS: &str = include_str!("../web/vendor/addon-fit.js");
/// LLM answers kept for the page
const LLM_KEPT: usize = 20;

/// a session is forgotten after a week without use
const SESSION_IDLE: Duration = Duration::from_secs(7 * 24 * 3600);
const MAX_BODY: usize = 64 * 1024;

pub struct Web {
    pub live: Arc<Live>,
    pub bus: Bus,
    pub config_path: PathBuf,
    /// the running configuration (main publishes every change)
    pub config: watch::Receiver<Config>,
    /// ask main to apply the saved configuration (as SIGHUP does)
    pub reload: mpsc::Sender<()>,
    pub version: String,
    sessions: Mutex<HashMap<String, Session>>,
    /// the last "media" event (rates, detections), for the status page
    last_media: Mutex<Option<serde_json::Value>>,
    /// the last "mqtt" event (connected, error), for the MQTT page
    last_mqtt: Mutex<Option<serde_json::Value>>,
    /// the last "llm" events, newest first
    last_llm: Mutex<std::collections::VecDeque<serde_json::Value>>,
    /// system update from the page (one for the life of the daemon)
    updater: &'static crate::update::Updater,
    #[cfg(feature = "webrtc")]
    webrtc: Arc<crate::webrtc::Sessions>,
}

struct Session {
    user: String,
    seen: Instant,
}

impl Web {
    pub fn new(live: Arc<Live>, bus: Bus, config_path: PathBuf, config: watch::Receiver<Config>,
               reload: mpsc::Sender<()>, version: String) -> Arc<Web> {
        Arc::new(Web {
            live,
            bus,
            config_path,
            config,
            reload,
            version,
            sessions: Mutex::new(HashMap::new()),
            last_media: Mutex::new(None),
            last_mqtt: Mutex::new(None),
            last_llm: Mutex::default(),
            updater: Box::leak(Box::new(crate::update::Updater::new())),
            #[cfg(feature = "webrtc")]
            webrtc: Arc::default(),
        })
    }
}

/// Serve until the task is dropped.
pub async fn serve(web: Arc<Web>, port: u16) {
    let listener = match TcpListener::bind(("0.0.0.0", port)).await {
        Ok(l) => l,
        Err(e) => {
            crate::error!("web: port {port}: {e}");
            return;
        }
    };
    crate::info!("web on port {port}");

    // keep the last media event for /api/status
    let w = web.clone();
    let mut rx = web.bus.subscribe();
    let keeper = tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(line) if line.contains("\"type\":\"media\"") => {
                    if let (Ok(v), Ok(mut m)) = (serde_json::from_str(&line), w.last_media.lock()) {
                        *m = Some(v);
                    }
                }
                Ok(line) if line.contains("\"type\":\"mqtt\"") => {
                    if let (Ok(v), Ok(mut m)) = (serde_json::from_str(&line), w.last_mqtt.lock()) {
                        *m = Some(v);
                    }
                }
                Ok(line) if line.contains("\"type\":\"llm\"") => {
                    if let (Ok(v), Ok(mut m)) = (serde_json::from_str(&line), w.last_llm.lock()) {
                        m.push_front(v);
                        m.truncate(LLM_KEPT);
                    }
                }
                Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => return,
            }
        }
    });
    let _keeper = AbortOnDrop(keeper);

    loop {
        let Ok((stream, _)) = listener.accept().await else { continue };
        let w = web.clone();
        // the address the browser reached us on: the WebRTC host candidate
        let local = stream.local_addr().map(|a| a.ip()).unwrap_or(IpAddr::from([0, 0, 0, 0]));
        tokio::spawn(async move {
            let svc = service_fn(move |req| handle(w.clone(), req, local));
            let _ = hyper::server::conn::http1::Builder::new()
                .serve_connection(TokioIo::new(stream), svc)
                .with_upgrades()
                .await;
        });
    }
}

pub struct AbortOnDrop(pub tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn handle(w: Arc<Web>, req: Request<Incoming>, local: IpAddr) -> Result<Response<RespBody>, Infallible> {
    let path = req.uri().path().to_string();
    let method = req.method().clone();
    let r = match (&method, path.as_str()) {
        (&Method::GET, "/") | (&Method::GET, "/index.html") => text(INDEX_HTML, "text/html; charset=utf-8"),
        (&Method::GET, "/app.js") => text(APP_JS, "text/javascript; charset=utf-8"),
        (&Method::GET, "/app.css") => text(APP_CSS, "text/css; charset=utf-8"),
        (&Method::GET, "/vendor/xterm.js") => cached(text(XTERM_JS, "text/javascript; charset=utf-8")),
        (&Method::GET, "/vendor/addon-fit.js") => cached(text(XTERM_FIT_JS, "text/javascript; charset=utf-8")),
        (&Method::GET, "/vendor/xterm.css") => cached(text(XTERM_CSS, "text/css; charset=utf-8")),
        (&Method::POST, "/api/login") => login(&w, req).await,
        _ => match session_user(&w, &req) {
            None => json(StatusCode::UNAUTHORIZED, &serde_json::json!({ "error": "login required" })),
            Some(user) => match (&method, path.as_str()) {
                (&Method::POST, "/api/logout") => logout(&w, &req),
                (&Method::GET, "/api/session") => json(StatusCode::OK, &serde_json::json!({ "user": user })),
                (&Method::GET, "/api/config") => {
                    // the saved configuration, not the running one: after a
                    // save the daemon applies it a moment later (a model
                    // change restarts the pipeline first), and a page that
                    // read the running one then saved it back again undid
                    // the change (a model switch reverted by the next slider)
                    json(StatusCode::OK, &saved_config(&w))
                }
                (&Method::POST, "/api/config") => set_config(&w, req).await,
                (&Method::GET, "/api/status") => json(StatusCode::OK, &status(&w)),
                (&Method::GET, "/api/update") => {
                    let slots = crate::update::slots();
                    let (cur, other) = match &slots {
                        Ok((c, o)) => (Some(*c), Some(*o)),
                        Err(_) => (None, None),
                    };
                    let st = w.updater.state();
                    // the other slot's version, unless it is being written
                    let other_version = other.filter(|_| !matches!(st.phase.as_str(), "receiving" | "verifying" | "finishing"))
                        .and_then(crate::update::slot_version);
                    json(StatusCode::OK, &serde_json::json!({
                        "running": cur, "inactive": other,
                        "running_version": cur.and_then(crate::update::slot_version),
                        "inactive_version": other_version,
                        "error": slots.err(),
                        "state": st,
                    }))
                }
                (&Method::POST, "/api/update") => update_upload(&w, &user, req).await,
                (&Method::POST, "/api/update/reboot") => {
                    let st = w.updater.state();
                    match st.target.as_deref().filter(|_| st.phase == "ready") {
                        Some(t) => match crate::update::switch_to(t) {
                            Ok(()) => {
                                crate::warn!("web: {user} reboots into the updated slot {t}");
                                tokio::spawn(async {
                                    tokio::time::sleep(Duration::from_secs(1)).await;
                                    let _ = std::process::Command::new("/sbin/reboot").status();
                                });
                                json(StatusCode::OK, &serde_json::json!({ "rebooting": t }))
                            }
                            Err(e) => json(StatusCode::INTERNAL_SERVER_ERROR, &serde_json::json!({ "error": e })),
                        },
                        None => json(StatusCode::CONFLICT, &serde_json::json!({ "error": "no update ready to boot" })),
                    }
                }
                (&Method::GET, "/api/model") => {
                    let m = w.live.model.lock().map(|m| m.clone()).unwrap_or_else(|_| Arc::from("{}"));
                    let mut r = Response::new(full(m.to_string()));
                    r.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
                    r.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
                    r
                }
                (&Method::GET, "/api/labels") => {
                    let m = media::model_path(&saved_config(&w).detection.model);
                    json(StatusCode::OK, &media::load_labels(&m))
                }
                (&Method::GET, "/api/models") => {
                    let active = saved_config(&w).detection.model;
                    json(StatusCode::OK, &system::models(&active))
                }
                (&Method::PUT, p) if p.starts_with("/api/models/") => {
                    let name = p["/api/models/".len()..].to_string();
                    upload_model(req, &name).await
                }
                (&Method::DELETE, p) if p.starts_with("/api/models/") => {
                    let active = w.config.borrow().detection.model.clone();
                    match system::delete_model(&p["/api/models/".len()..], &active) {
                        Ok(()) => json(StatusCode::OK, &serde_json::json!({})),
                        Err(e) => json(StatusCode::BAD_REQUEST, &serde_json::json!({ "error": e })),
                    }
                }
                (&Method::POST, "/api/password") => password(&user, req).await,
                (&Method::POST, "/api/reboot") => {
                    crate::warn!("web: reboot asked by {user}");
                    tokio::spawn(async {
                        tokio::time::sleep(Duration::from_secs(1)).await;
                        let _ = std::process::Command::new("/sbin/reboot").status();
                    });
                    json(StatusCode::OK, &serde_json::json!({ "rebooting": true }))
                }
                (&Method::GET, "/api/logs") => text_owned(system::log_tail(300)),
                (&Method::GET, "/api/crashes") => json(StatusCode::OK, &system::crashes()),
                (&Method::GET, p) if p.starts_with("/api/crashes/") => {
                    let mut parts = p["/api/crashes/".len()..].splitn(2, '/');
                    match (parts.next(), parts.next()) {
                        (Some(d), Some(f)) => match system::crash_file(d, f) {
                            Some(t) => text_owned(t),
                            None => json(StatusCode::NOT_FOUND, &serde_json::json!({ "error": "not found" })),
                        },
                        _ => json(StatusCode::NOT_FOUND, &serde_json::json!({ "error": "not found" })),
                    }
                }
                (&Method::GET, "/api/timezones") => json(StatusCode::OK, &system::timezones()),
                (&Method::POST, "/api/mqtt/test") => {
                    match read_body(req).await.and_then(|b| {
                        serde_json::from_slice::<config::Mqtt>(&b).map_err(|e| e.to_string())
                    }) {
                        Ok(m) => match crate::mqtt::test(&m).await {
                            Ok(()) => json(StatusCode::OK, &serde_json::json!({ "ok": true })),
                            Err(e) => json(StatusCode::OK, &serde_json::json!({ "ok": false, "error": e })),
                        },
                        Err(e) => json(StatusCode::BAD_REQUEST, &serde_json::json!({ "error": e })),
                    }
                }
                (&Method::GET, "/api/llm") => {
                    let last: Vec<_> = w.last_llm.lock().map(|m| m.iter().cloned().collect()).unwrap_or_default();
                    json(StatusCode::OK, &last)
                }
                (&Method::POST, "/api/llm/test") => llm_test(&w, req).await,
                (&Method::GET, "/api/terminal") if !w.config.borrow().web.terminal => {
                    json(StatusCode::FORBIDDEN, &serde_json::json!({ "error": "the terminal is off (System page)" }))
                }
                (&Method::GET, "/api/terminal") => match crate::terminal::upgrade(req, user) {
                    Ok(r) => r.map(full),
                    Err((code, e)) => json(code, &serde_json::json!({ "error": e })),
                },
                (&Method::GET, "/api/mqtt") => {
                    let enabled = w.config.borrow().mqtt.enabled;
                    let state = w.last_mqtt.lock().ok().and_then(|m| m.clone());
                    json(StatusCode::OK, &serde_json::json!({
                        "enabled": enabled, "device_id": crate::mqtt::device_id(), "state": state,
                    }))
                }
                (&Method::GET, "/api/network") => {
                    let addrs: Vec<_> =
                        addresses().into_iter().map(|(n, a)| serde_json::json!({ "if": n, "ip": a })).collect();
                    json(StatusCode::OK, &serde_json::json!({ "addresses": addrs, "wifi": network::wifi_status().await }))
                }
                (&Method::POST, "/api/network/scan") => match network::scan().await {
                    Ok(v) => json(StatusCode::OK, &v),
                    Err(e) => json(StatusCode::SERVICE_UNAVAILABLE, &serde_json::json!({ "error": e })),
                },
                (&Method::POST, "/api/network/connect") => wifi_connect(&user, req).await,
                (&Method::POST, "/api/network/forget") => {
                    #[derive(serde::Deserialize)]
                    struct F {
                        id: u32,
                    }
                    match read_body(req).await.and_then(|b| serde_json::from_slice::<F>(&b).map_err(|e| e.to_string())) {
                        Ok(f) => match network::forget(f.id).await {
                            Ok(()) => json(StatusCode::OK, &serde_json::json!({})),
                            Err(e) => json(StatusCode::BAD_REQUEST, &serde_json::json!({ "error": e })),
                        },
                        Err(e) => json(StatusCode::BAD_REQUEST, &serde_json::json!({ "error": e })),
                    }
                }
                (&Method::GET, "/stream.mjpg") => mjpeg(&w),
                (&Method::GET, "/snapshot.jpg") => snapshot(&w).await,
                (&Method::GET, "/live") => live(&w),
                #[cfg(feature = "webrtc")]
                (&Method::POST, "/whep") => whep(&w, req, local).await,
                #[cfg(feature = "webrtc")]
                (&Method::DELETE, p) if p.starts_with("/whep/") => {
                    let found = p[6..].parse().is_ok_and(|id| w.webrtc.close(id));
                    json(if found { StatusCode::OK } else { StatusCode::NOT_FOUND }, &serde_json::json!({}))
                }
                _ => json(StatusCode::NOT_FOUND, &serde_json::json!({ "error": "not found" })),
            },
        },
    };
    Ok(r)
}

fn full(b: impl Into<Bytes>) -> RespBody {
    Full::new(b.into()).boxed()
}

fn text(s: &'static str, ctype: &'static str) -> Response<RespBody> {
    let mut r = Response::new(full(s));
    r.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static(ctype));
    r
}

/// the vendored files change only with the binary
fn cached(mut r: Response<RespBody>) -> Response<RespBody> {
    r.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("max-age=86400"));
    r
}

fn text_owned(s: String) -> Response<RespBody> {
    let mut r = Response::new(full(s));
    r.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("text/plain; charset=utf-8"));
    r.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    r
}

fn json<T: serde::Serialize>(status: StatusCode, v: &T) -> Response<RespBody> {
    let body = serde_json::to_vec(v).unwrap_or_else(|_| b"{}".to_vec());
    let mut r = Response::new(full(body));
    *r.status_mut() = status;
    r.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
    r.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    r
}

async fn read_body(req: Request<Incoming>) -> Result<Bytes, String> {
    Limited::new(req.into_body(), MAX_BODY)
        .collect()
        .await
        .map(|c| c.to_bytes())
        .map_err(|e| format!("body: {e}"))
}

// ---- login --------------------------------------------------------------

unsafe extern "C" {
    /// musl libc crypt(3): $5$ (SHA-256) and $6$ (SHA-512) hashes
    fn crypt(key: *const c_char, salt: *const c_char) -> *mut c_char;
}

/// The password of a system account, checked against /etc/shadow (the page
/// login is the system login, owner's decision of 20/10/2026).
pub fn check_password(user: &str, password: &str) -> bool {
    let Ok(shadow) = std::fs::read_to_string("/etc/shadow") else { return false };
    let Some(hash) = shadow.lines().find_map(|l| {
        let mut f = l.split(':');
        (f.next() == Some(user)).then(|| f.next().unwrap_or("").to_string())
    }) else {
        return false;
    };
    if !hash.starts_with('$') {
        return false; // locked or no password
    }
    let (Ok(k), Ok(h)) = (CString::new(password), CString::new(hash.as_str())) else { return false };
    // SAFETY: valid C strings; crypt's static result is copied before the
    // next call (the network side is one thread)
    let r = unsafe { crypt(k.as_ptr(), h.as_ptr()) };
    !r.is_null() && unsafe { CStr::from_ptr(r) }.to_bytes() == hash.as_bytes()
}

fn new_token() -> Option<String> {
    use std::io::Read;
    let mut b = [0u8; 16];
    std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut b)).ok()?;
    Some(b.iter().map(|x| format!("{x:02x}")).collect())
}

fn cookie_sid(req: &Request<Incoming>) -> Option<String> {
    let c = req.headers().get(header::COOKIE)?.to_str().ok()?;
    c.split(';').find_map(|kv| kv.trim().strip_prefix("sid=").map(String::from))
}

fn session_user(w: &Web, req: &Request<Incoming>) -> Option<String> {
    let sid = cookie_sid(req)?;
    let mut s = w.sessions.lock().ok()?;
    let now = Instant::now();
    s.retain(|_, v| now.duration_since(v.seen) < SESSION_IDLE);
    let v = s.get_mut(&sid)?;
    v.seen = now;
    Some(v.user.clone())
}

async fn login(w: &Web, req: Request<Incoming>) -> Response<RespBody> {
    #[derive(serde::Deserialize)]
    struct L {
        user: String,
        password: String,
    }
    let l = match read_body(req).await.and_then(|b| serde_json::from_slice::<L>(&b).map_err(|e| e.to_string())) {
        Ok(l) => l,
        Err(e) => return json(StatusCode::BAD_REQUEST, &serde_json::json!({ "error": e })),
    };
    if !check_password(&l.user, &l.password) {
        crate::warn!("web: failed login for {:?}", l.user);
        // slow down guessing
        tokio::time::sleep(Duration::from_secs(1)).await;
        return json(StatusCode::UNAUTHORIZED, &serde_json::json!({ "error": "wrong user or password" }));
    }
    let Some(tok) = new_token() else {
        return json(StatusCode::INTERNAL_SERVER_ERROR, &serde_json::json!({ "error": "no random source" }));
    };
    if let Ok(mut s) = w.sessions.lock() {
        s.insert(tok.clone(), Session { user: l.user.clone(), seen: Instant::now() });
    }
    crate::info!("web: {} logged in", l.user);
    let mut r = json(StatusCode::OK, &serde_json::json!({ "user": l.user }));
    let c = format!("sid={tok}; HttpOnly; SameSite=Strict; Path=/; Max-Age={}", SESSION_IDLE.as_secs());
    if let Ok(v) = HeaderValue::from_str(&c) {
        r.headers_mut().insert(header::SET_COOKIE, v);
    }
    r
}

fn logout(w: &Web, req: &Request<Incoming>) -> Response<RespBody> {
    if let (Some(sid), Ok(mut s)) = (cookie_sid(req), w.sessions.lock()) {
        s.remove(&sid);
    }
    let mut r = json(StatusCode::OK, &serde_json::json!({}));
    r.headers_mut().insert(header::SET_COOKIE, HeaderValue::from_static("sid=; Path=/; Max-Age=0"));
    r
}

// ---- models and password ---------------------------------------------------

/// PUT /api/models/<name>: the body is the file, written to disk as it
/// comes (a model is tens of MB: not kept in memory), checked, put in place.
async fn upload_model(req: Request<Incoming>, name: &str) -> Response<RespBody> {
    use std::io::Write;
    let bad = |e: String| json(StatusCode::BAD_REQUEST, &serde_json::json!({ "error": e }));
    if !system::model_name_ok(name) {
        return bad(format!("{name:?}: a .cvimodel or .json name (letters, digits, . _ -)"));
    }
    let tmp = system::model_tmp(name);
    // plain writes: chunks of tens of KB to the eMMC, short enough for the
    // network thread
    let mut f = match std::fs::File::create(&tmp) {
        Ok(f) => f,
        Err(e) => return bad(e.to_string()),
    };
    let mut body = req.into_body();
    let mut total = 0u64;
    while let Some(frame) = body.frame().await {
        let chunk = match frame {
            Ok(fr) => match fr.into_data() {
                Ok(d) => d,
                Err(_) => continue,
            },
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                return bad(format!("upload: {e}"));
            }
        };
        total += chunk.len() as u64;
        if total > system::MAX_MODEL {
            let _ = std::fs::remove_file(&tmp);
            return bad(format!("{name}: larger than {} MB", system::MAX_MODEL >> 20));
        }
        if let Err(e) = f.write_all(&chunk) {
            let _ = std::fs::remove_file(&tmp);
            return bad(e.to_string());
        }
    }
    drop(f);
    match system::finish_model(name) {
        Ok(()) => {
            crate::info!("web: model {name} uploaded ({total} bytes)");
            json(StatusCode::OK, &serde_json::json!({ "name": name, "size": total }))
        }
        Err(e) => bad(e),
    }
}

/// The configuration as saved (what the page last chose), not the running
/// one: the daemon applies a save a moment later (a model change restarts
/// the pipeline first), and pages that read the running one in between got
/// the old model back (and saved it again with the next slider).
fn saved_config(w: &Web) -> Config {
    match config::load(&w.config_path) {
        Ok(config::Loaded::File(c)) | Ok(config::Loaded::Created(c)) => c,
        Err(_) => w.config.borrow().clone(),
    }
}

/// POST /api/update: the body is a recamera-update-*.rcu, handed chunk by
/// chunk to the installer thread (update.rs), which writes the inactive slot
/// while it arrives.
async fn update_upload(w: &Arc<Web>, user: &str, req: Request<Incoming>) -> Response<RespBody> {
    let busy = match w.updater.begin() {
        Ok(b) => b,
        Err(e) => return json(StatusCode::CONFLICT, &serde_json::json!({ "error": e })),
    };
    crate::warn!("web: {user} uploads a system update");
    let (tx, rx) = mpsc::channel::<Bytes>(16);
    let up = w.updater;
    let worker = tokio::task::spawn_blocking(move || crate::update::install(up, rx));
    let mut body = req.into_body();
    let mut upload_err = None;
    while let Some(frame) = body.frame().await {
        match frame {
            Ok(fr) => {
                if let Ok(d) = fr.into_data()
                    && tx.send(d).await.is_err()
                {
                    break; // the installer stopped (an error it reports)
                }
            }
            Err(e) => {
                upload_err = Some(format!("upload interrupted: {e}"));
                break;
            }
        }
    }
    drop(tx);
    let r = worker.await.unwrap_or_else(|e| Err(format!("installer: {e}")));
    drop(busy);
    match (r, upload_err) {
        (Ok(v), _) => json(StatusCode::OK, &serde_json::json!({ "version": v, "state": w.updater.state() })),
        (Err(e), Some(u)) => json(StatusCode::BAD_REQUEST, &serde_json::json!({ "error": format!("{u} ({e})") })),
        (Err(e), None) => json(StatusCode::BAD_REQUEST, &serde_json::json!({ "error": e })),
    }
}

async fn wifi_connect(user: &str, req: Request<Incoming>) -> Response<RespBody> {
    #[derive(serde::Deserialize)]
    struct C {
        ssid: String,
        #[serde(default)]
        psk: String,
    }
    let c = match read_body(req).await.and_then(|b| serde_json::from_slice::<C>(&b).map_err(|e| e.to_string())) {
        Ok(c) => c,
        Err(e) => return json(StatusCode::BAD_REQUEST, &serde_json::json!({ "error": e })),
    };
    match network::connect(&c.ssid, &c.psk).await {
        Ok(()) => {
            crate::info!("web: {user} set Wi-Fi network {:?}", c.ssid);
            json(StatusCode::OK, &serde_json::json!({}))
        }
        Err(e) => json(StatusCode::BAD_REQUEST, &serde_json::json!({ "error": e })),
    }
}

async fn password(user: &str, req: Request<Incoming>) -> Response<RespBody> {
    #[derive(serde::Deserialize)]
    struct P {
        current: String,
        new: String,
    }
    let p = match read_body(req).await.and_then(|b| serde_json::from_slice::<P>(&b).map_err(|e| e.to_string())) {
        Ok(p) => p,
        Err(e) => return json(StatusCode::BAD_REQUEST, &serde_json::json!({ "error": e })),
    };
    if !check_password(user, &p.current) {
        tokio::time::sleep(Duration::from_secs(1)).await;
        return json(StatusCode::FORBIDDEN, &serde_json::json!({ "error": "the current password is wrong" }));
    }
    match system::set_password(user, &p.new) {
        Ok(()) => {
            crate::info!("web: password of {user} changed");
            json(StatusCode::OK, &serde_json::json!({}))
        }
        Err(e) => json(StatusCode::BAD_REQUEST, &serde_json::json!({ "error": e })),
    }
}

// ---- LLM ------------------------------------------------------------------

/// POST /api/llm/test: the form's settings (not saved) with the current
/// frame; the answer and the picture that was sent.
async fn llm_test(w: &Arc<Web>, req: Request<Incoming>) -> Response<RespBody> {
    use base64::Engine;
    let c = match read_body(req).await.and_then(|b| serde_json::from_slice::<config::Llm>(&b).map_err(|e| e.to_string()))
    {
        Ok(c) => c,
        Err(e) => return json(StatusCode::BAD_REQUEST, &serde_json::json!({ "error": e })),
    };
    let e = c.endpoint.trim();
    if !["http://", "https://", "ws://", "wss://"].iter().any(|p| e.starts_with(p)) {
        return json(StatusCode::BAD_REQUEST, &serde_json::json!({ "error": "endpoint: an http(s):// or ws(s):// URL" }));
    }
    let Some(jpg) = w.live.snapshot().await else {
        return json(StatusCode::SERVICE_UNAVAILABLE, &serde_json::json!({ "error": "no frame from the camera" }));
    };
    let det = w.config.borrow().detection.clone();
    let label = media::load_labels(&media::model_path(&det.model))
        .get(det.target_class as usize)
        .cloned()
        .unwrap_or_else(|| "target".into());
    let stand_in = serde_json::json!({ "label": label, "test": true });
    let r = crate::llm::ask(&c, &jpg, &label, &stand_in).await;
    crate::llm::report(&w.bus, "test", &label, &r);
    let image = format!("data:image/jpeg;base64,{}", base64::engine::general_purpose::STANDARD.encode(&jpg));
    match r {
        Ok(a) => json(StatusCode::OK, &serde_json::json!({ "ok": true, "text": a.text, "ms": a.ms, "image": image })),
        Err(e) => json(StatusCode::OK, &serde_json::json!({ "ok": false, "error": e, "image": image })),
    }
}

// ---- configuration and status -------------------------------------------

async fn set_config(w: &Web, req: Request<Incoming>) -> Response<RespBody> {
    let c = match read_body(req).await.and_then(|b| serde_json::from_slice::<Config>(&b).map_err(|e| e.to_string()))
    {
        Ok(c) => c,
        Err(e) => return json(StatusCode::BAD_REQUEST, &serde_json::json!({ "error": e })),
    };
    if let Err(e) = c.validate() {
        return json(StatusCode::BAD_REQUEST, &serde_json::json!({ "error": e }));
    }
    if let Err(e) = config::save(&w.config_path, &c) {
        return json(StatusCode::INTERNAL_SERVER_ERROR, &serde_json::json!({ "error": e.to_string() }));
    }
    let _ = w.reload.send(()).await;
    json(StatusCode::OK, &serde_json::json!({ "saved": true }))
}

/// IPv4 address of each interface that has one
pub fn addresses() -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut ifa: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: getifaddrs fills a list freed below; only AF_INET entries are
    // read as sockaddr_in
    unsafe {
        if libc::getifaddrs(&mut ifa) != 0 {
            return out;
        }
        let mut p = ifa;
        while !p.is_null() {
            let a = (*p).ifa_addr;
            if !a.is_null() && (*a).sa_family as i32 == libc::AF_INET {
                let sin = &*(a as *const libc::sockaddr_in);
                let ip = std::net::Ipv4Addr::from(u32::from_be(sin.sin_addr.s_addr));
                let name = CStr::from_ptr((*p).ifa_name).to_string_lossy().into_owned();
                if !ip.is_loopback() {
                    out.push((name, ip.to_string()));
                }
            }
            p = (*p).ifa_next;
        }
        libc::freeifaddrs(ifa);
    }
    out
}

fn status(w: &Web) -> serde_json::Value {
    let cmdline = std::fs::read_to_string("/proc/cmdline").unwrap_or_default();
    let slot = cmdline.split_whitespace().find_map(|a| a.strip_prefix("recamera.slot=")).unwrap_or("?");
    let media = w.last_media.lock().ok().and_then(|m| m.clone());
    let now_ms = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    serde_json::json!({
        "version": w.version,
        "now_ms": now_ms,
        "slot": slot,
        "addresses": addresses().into_iter().map(|(n, a)| serde_json::json!({ "if": n, "ip": a }))
            .collect::<Vec<_>>(),
        "system": telemetry::sample(),
        "media": media,
    })
}

// ---- streams --------------------------------------------------------------

/// A response body fed by a channel (the stream tasks write into it).
struct ChanBody(mpsc::Receiver<Bytes>);

impl Body for ChanBody {
    type Data = Bytes;
    type Error = Infallible;

    fn poll_frame(mut self: Pin<&mut Self>, cx: &mut Context<'_>)
        -> Poll<Option<Result<Frame<Bytes>, Infallible>>> {
        self.0.poll_recv(cx).map(|o| o.map(|b| Ok(Frame::data(b))))
    }
}

fn streaming(ctype: &'static str) -> (mpsc::Sender<Bytes>, Response<RespBody>) {
    let (tx, rx) = mpsc::channel(2);
    let mut r = Response::new(BoxBody::new(ChanBody(rx)));
    r.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static(ctype));
    r.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    (tx, r)
}

fn mjpeg(w: &Arc<Web>) -> Response<RespBody> {
    let (tx, r) = streaming("multipart/x-mixed-replace; boundary=frame");
    let viewer = w.live.viewer();
    let mut rx = w.live.jpeg.subscribe();
    tokio::spawn(async move {
        let _viewer = viewer; // the encoder runs while this task lives
        while rx.changed().await.is_ok() {
            let jpg = rx.borrow_and_update().clone();
            if jpg.is_empty() {
                continue;
            }
            let head = format!("--frame\r\nContent-Type: image/jpeg\r\nContent-Length: {}\r\n\r\n", jpg.len());
            if tx.send(Bytes::from(head)).await.is_err()
                || tx.send(jpg).await.is_err()
                || tx.send(Bytes::from_static(b"\r\n")).await.is_err()
            {
                return; // the browser went away
            }
        }
    });
    r
}

async fn snapshot(w: &Arc<Web>) -> Response<RespBody> {
    match w.live.snapshot().await {
        Some(jpg) => {
            let mut r = Response::new(full(jpg));
            r.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("image/jpeg"));
            r.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
            r
        }
        None => json(StatusCode::SERVICE_UNAVAILABLE, &serde_json::json!({ "error": "no frame" })),
    }
}

/// Server-Sent Events: "dets" (boxes of every frame) and the daemon events.
fn live(w: &Arc<Web>) -> Response<RespBody> {
    let (tx, r) = streaming("text/event-stream");
    let mut dets = w.live.dets.subscribe();
    let mut events = w.bus.subscribe();
    tokio::spawn(async move {
        loop {
            let msg = tokio::select! {
                r = dets.changed() => match r {
                    Ok(()) => format!("event: dets\ndata: {}\n\n", &**dets.borrow_and_update()),
                    Err(_) => return,
                },
                r = events.recv() => match r {
                    Ok(line) => format!("data: {}\n\n", line.trim_end()),
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => return,
                },
            };
            if tx.send(Bytes::from(msg)).await.is_err() {
                return;
            }
        }
    });
    r
}

// ---- WebRTC ---------------------------------------------------------------

/// WHEP: the body is the browser's SDP offer, the answer comes back with 201
/// and the session's URL (DELETE it to stop).
#[cfg(feature = "webrtc")]
async fn whep(w: &Arc<Web>, req: Request<Incoming>, local: IpAddr) -> Response<RespBody> {
    let offer = match read_body(req).await.and_then(|b| String::from_utf8(b.to_vec()).map_err(|e| e.to_string())) {
        Ok(o) => o,
        Err(e) => return json(StatusCode::BAD_REQUEST, &serde_json::json!({ "error": e })),
    };
    match w.webrtc.open(&w.live, &offer, local).await {
        Ok((id, answer)) => {
            let mut r = Response::new(full(answer));
            *r.status_mut() = StatusCode::CREATED;
            r.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("application/sdp"));
            if let Ok(v) = HeaderValue::from_str(&format!("/whep/{id}")) {
                r.headers_mut().insert(header::LOCATION, v);
            }
            r
        }
        Err(e) => {
            crate::warn!("webrtc: {e}");
            json(StatusCode::SERVICE_UNAVAILABLE, &serde_json::json!({ "error": e }))
        }
    }
}

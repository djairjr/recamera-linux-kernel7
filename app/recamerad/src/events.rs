//! Events: one JSON object per line, with "type" and "ts" (Unix time in ms),
//! on stdout (--events, for pipes: `recamerad -f --events | jq`) and on a Unix
//! socket (every client gets the stream: `recamera-events`).
//! docs/application.md §2. A slow client only loses events; it never
//! holds the daemon back.

use serde::Serialize;
use serde_json::{Map, Value};
use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::io::AsyncWriteExt;
use tokio::net::UnixListener;
use tokio::sync::broadcast;

/// lines kept for a client that falls behind
const BACKLOG: usize = 64;

#[derive(Clone)]
pub struct Bus {
    tx: broadcast::Sender<Arc<str>>,
}

impl Bus {
    pub fn new() -> Self {
        Bus { tx: broadcast::channel(BACKLOG).0 }
    }

    /// Publish an event: `fields` (a serializable struct or map) plus
    /// "type" and "ts".
    pub fn emit<T: Serialize>(&self, kind: &str, fields: &T) {
        let mut obj = match serde_json::to_value(fields) {
            Ok(Value::Object(m)) => m,
            Ok(Value::Null) => Map::new(),
            Ok(v) => Map::from_iter([("value".to_string(), v)]),
            Err(e) => {
                crate::error!("event {kind}: {e}");
                return;
            }
        };
        let ts = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
        obj.insert("type".into(), kind.into());
        obj.insert("ts".into(), ts.into());
        let mut line = Value::Object(obj).to_string();
        line.push('\n');
        // no receivers is fine: nobody is listening right now
        let _ = self.tx.send(Arc::from(line));
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Arc<str>> {
        self.tx.subscribe()
    }
}

/// Copy the events to stdout until the daemon stops.
pub async fn to_stdout(bus: Bus) {
    let mut rx = bus.subscribe();
    let mut out = tokio::io::stdout();
    loop {
        match rx.recv().await {
            Ok(line) => {
                if out.write_all(line.as_bytes()).await.is_err() || out.flush().await.is_err() {
                    return; // stdout closed (the pipe reader went away)
                }
            }
            Err(broadcast::error::RecvError::Lagged(n)) => crate::warn!("stdout: {n} events dropped"),
            Err(broadcast::error::RecvError::Closed) => return,
        }
    }
}

/// Serve the event stream on a Unix socket; each client gets its own copy.
pub async fn serve_socket(bus: Bus, path: String) {
    let p = Path::new(&path);
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::remove_file(p); // a stale socket from a previous run
    let listener = match UnixListener::bind(p) {
        Ok(l) => l,
        Err(e) => {
            crate::error!("events socket {path}: {e}");
            return;
        }
    };
    crate::info!("events on {path}");
    loop {
        let Ok((mut sock, _)) = listener.accept().await else { continue };
        let mut rx = bus.subscribe();
        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(line) => {
                        if sock.write_all(line.as_bytes()).await.is_err() {
                            return; // client gone
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        crate::debug!("events client lagged, {n} dropped")
                    }
                    Err(broadcast::error::RecvError::Closed) => return,
                }
            }
        });
    }
}

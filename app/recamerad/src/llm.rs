//! The frame of every arrival to an LLM/VLM in the cloud (stage A8b), with
//! the detection; the answer comes back as an "llm" event (page, MQTT,
//! event socket). Switched on and off on the page.
//!
//! Two kinds of endpoint, by the scheme of the URL:
//!
//! - `http(s)://…/chat/completions`: the OpenAI chat API, which OpenAI,
//!   Ollama (local and cloud), OpenRouter, vLLM and most providers speak. One
//!   user message: the prompt, the detection and the picture as a
//!   `data:image/jpeg;base64,…` URL; the answer is `choices[0].message.content`.
//! - `ws(s)://…`: a service of your own. One JSON text message out
//!   (`{"type":"detection", "model", "prompt", "max_tokens", "detection",
//!   "image"}`), the first message back is the answer (its "text", "answer"
//!   or "content" field when it is JSON, else the whole message).
//!
//! The key goes as `Authorization: Bearer <key>` on both. TLS by the image's
//! OpenSSL with the system CA bundle (native-tls).

use crate::config;
use crate::events::Bus;
use crate::live::Live;
use base64::Engine;
use bytes::Bytes;
use http_body_util::{BodyExt, Full, Limited};
use hyper::{Request, Uri};
use hyper_util::rt::TokioIo;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio::sync::broadcast;

/// the largest answer read
const MAX_ANSWER: usize = 1 << 20;

/// One answer.
pub struct Answer {
    pub text: String,
    pub ms: u64,
}

/// Run until the task is dropped: every arrival (at most one request per
/// min_interval_s, one at a time) goes to the endpoint.
pub async fn run(c: config::Llm, bus: Bus, live: Arc<Live>) {
    crate::info!("llm: arrivals to {}", c.endpoint);
    let c = Arc::new(c);
    let busy = Arc::new(AtomicBool::new(false));
    let mut last: Option<Instant> = None;
    let mut rx = bus.subscribe();
    loop {
        let line = match rx.recv().await {
            Ok(l) => l,
            Err(broadcast::error::RecvError::Lagged(_)) => continue,
            Err(broadcast::error::RecvError::Closed) => return,
        };
        if !line.contains("\"type\":\"target_arrived\"") {
            continue;
        }
        let Ok(ev) = serde_json::from_str::<serde_json::Value>(&line) else { continue };
        if last.is_some_and(|t| t.elapsed() < Duration::from_secs(c.min_interval_s as u64)) {
            crate::debug!("llm: arrival within min_interval_s, not sent");
            continue;
        }
        if busy.swap(true, Ordering::AcqRel) {
            crate::debug!("llm: a request is still running, arrival not sent");
            continue;
        }
        last = Some(Instant::now());
        let (c, bus, live, busy) = (c.clone(), bus.clone(), live.clone(), busy.clone());
        tokio::spawn(async move {
            let label = ev.get("label").and_then(|l| l.as_str()).unwrap_or("target").to_string();
            let r = match live.snapshot().await {
                Some(jpg) => ask(&c, &jpg, &label, &ev).await,
                None => Err("no frame from the camera".into()),
            };
            report(&bus, "arrival", &label, &r);
            busy.store(false, Ordering::Release);
        });
    }
}

/// The "llm" event of an answer or an error.
pub fn report(bus: &Bus, trigger: &str, label: &str, r: &Result<Answer, String>) {
    match r {
        Ok(a) => {
            crate::info!("llm: answer in {} ms", a.ms);
            bus.emit("llm", &serde_json::json!({
                "ok": true, "trigger": trigger, "label": label, "text": a.text, "ms": a.ms,
            }));
        }
        Err(e) => {
            crate::warn!("llm: {e}");
            bus.emit("llm", &serde_json::json!({ "ok": false, "trigger": trigger, "label": label, "error": e }));
        }
    }
}

/// Ask about one frame; `detection` is the arrival event (or a stand-in).
pub async fn ask(c: &config::Llm, jpg: &[u8], label: &str, detection: &serde_json::Value)
    -> Result<Answer, String> {
    let t = Instant::now();
    let image = format!("data:image/jpeg;base64,{}", base64::engine::general_purpose::STANDARD.encode(jpg));
    let prompt = c.prompt.replace("{label}", label);
    let endpoint = c.endpoint.trim();
    let fut = async {
        if endpoint.starts_with("ws://") || endpoint.starts_with("wss://") {
            ask_ws(c, endpoint, &prompt, &image, detection).await
        } else {
            ask_openai(c, endpoint, &prompt, &image, detection).await
        }
    };
    let text = tokio::time::timeout(Duration::from_secs(c.timeout_s as u64), fut)
        .await
        .map_err(|_| format!("no answer in {} s", c.timeout_s))??;
    Ok(Answer { text: text.trim().to_string(), ms: t.elapsed().as_millis() as u64 })
}

/// The detection the model reads next to the prompt: what the detector saw,
/// without the daemon's bookkeeping.
fn detection_note(d: &serde_json::Value) -> String {
    let mut d = d.clone();
    if let Some(o) = d.as_object_mut() {
        o.remove("type");
        o.remove("ts");
    }
    format!("Detector output (box corners as fractions of the picture): {d}")
}

// ---- OpenAI chat API --------------------------------------------------------

async fn ask_openai(c: &config::Llm, url: &str, prompt: &str, image: &str, detection: &serde_json::Value)
    -> Result<String, String> {
    let mut body = serde_json::json!({
        "model": c.model,
        "max_tokens": c.max_tokens,
        "messages": [{
            "role": "user",
            "content": [
                { "type": "text", "text": format!("{prompt}\n\n{}", detection_note(detection)) },
                { "type": "image_url", "image_url": { "url": image } },
            ],
        }],
    });
    let mut r = post_json(url, &c.api_key, &body).await?;
    // the newer OpenAI models refuse max_tokens and want max_completion_tokens
    if let Err(e) = &r
        && e.contains("max_completion_tokens")
        && let Some(o) = body.as_object_mut()
    {
        o.remove("max_tokens");
        o.insert("max_completion_tokens".into(), c.max_tokens.into());
        r = post_json(url, &c.api_key, &body).await?;
    }
    let v = r?;
    v.pointer("/choices/0/message/content")
        .and_then(|t| t.as_str())
        .map(String::from)
        .ok_or_else(|| format!("unexpected answer: {}", clip(&v.to_string())))
}

/// POST a JSON body; Ok(Err(..)) is the server's refusal (status not 2xx).
async fn post_json(url: &str, key: &str, body: &serde_json::Value)
    -> Result<Result<serde_json::Value, String>, String> {
    let uri: Uri = url.parse().map_err(|e| format!("endpoint: {e}"))?;
    let tls = match uri.scheme_str() {
        Some("https") => true,
        Some("http") => false,
        _ => return Err("endpoint: http:// or https://".into()),
    };
    let host = uri.host().ok_or("endpoint: no host")?.to_string();
    let port = uri.port_u16().unwrap_or(if tls { 443 } else { 80 });
    let path = uri.path_and_query().map(|p| p.as_str()).unwrap_or("/").to_string();

    let mut req = Request::post(path)
        .header(hyper::header::HOST, if uri.port().is_some() { format!("{host}:{port}") } else { host.clone() })
        .header(hyper::header::CONTENT_TYPE, "application/json")
        .header(hyper::header::USER_AGENT, concat!("recamerad/", env!("CARGO_PKG_VERSION")));
    if !key.is_empty() {
        req = req.header(hyper::header::AUTHORIZATION, format!("Bearer {key}"));
    }
    let req = req
        .body(Full::new(Bytes::from(serde_json::to_vec(body).map_err(|e| e.to_string())?)))
        .map_err(|e| e.to_string())?;

    let tcp = TcpStream::connect((host.as_str(), port)).await.map_err(|e| format!("{host}:{port}: {e}"))?;
    let (status, bytes) = if tls {
        let cx = native_tls::TlsConnector::new().map_err(|e| format!("TLS: {e}"))?;
        let s = tokio_native_tls::TlsConnector::from(cx).connect(&host, tcp).await.map_err(|e| format!("TLS: {e}"))?;
        send(s, req).await?
    } else {
        send(tcp, req).await?
    };
    let v: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|_| format!("HTTP {status}: {}", clip(&String::from_utf8_lossy(&bytes))))?;
    if !status.is_success() {
        let msg = v.pointer("/error/message").or_else(|| v.get("error")).map(|m| match m.as_str() {
            Some(s) => s.to_string(),
            None => m.to_string(),
        });
        return Ok(Err(format!("HTTP {status}: {}", clip(&msg.unwrap_or_else(|| v.to_string())))));
    }
    Ok(Ok(v))
}

async fn send<S>(io: S, req: Request<Full<Bytes>>) -> Result<(hyper::StatusCode, Bytes), String>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (mut tx, conn) = hyper::client::conn::http1::handshake(TokioIo::new(io)).await.map_err(|e| e.to_string())?;
    let conn = crate::web::AbortOnDrop(tokio::spawn(async move {
        let _ = conn.await;
    }));
    let resp = tx.send_request(req).await.map_err(|e| e.to_string())?;
    let status = resp.status();
    let body = Limited::new(resp.into_body(), MAX_ANSWER).collect().await.map_err(|e| e.to_string())?.to_bytes();
    drop(conn);
    Ok((status, body))
}

// ---- WebSocket --------------------------------------------------------------

async fn ask_ws(c: &config::Llm, url: &str, prompt: &str, image: &str, detection: &serde_json::Value)
    -> Result<String, String> {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::{Message, client::IntoClientRequest};

    let mut req = url.into_client_request().map_err(|e| format!("endpoint: {e}"))?;
    if !c.api_key.is_empty() {
        let v = format!("Bearer {}", c.api_key).parse().map_err(|_| "api_key: not a header value".to_string())?;
        req.headers_mut().insert(hyper::header::AUTHORIZATION, v);
    }
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.map_err(|e| e.to_string())?;
    let msg = serde_json::json!({
        "type": "detection",
        "model": c.model,
        "prompt": prompt,
        "max_tokens": c.max_tokens,
        "detection": detection,
        "image": image,
    });
    ws.send(Message::text(msg.to_string())).await.map_err(|e| e.to_string())?;
    let text = loop {
        match ws.next().await {
            Some(Ok(Message::Text(t))) => break t.to_string(),
            Some(Ok(Message::Binary(b))) => break String::from_utf8_lossy(&b).into_owned(),
            Some(Ok(Message::Close(_))) | None => return Err("closed without an answer".into()),
            Some(Ok(_)) => continue, // ping, pong
            Some(Err(e)) => return Err(e.to_string()),
        }
    };
    let _ = ws.close(None).await;
    Ok(match serde_json::from_str::<serde_json::Value>(&text) {
        Ok(v) => ["text", "answer", "content"]
            .iter()
            .find_map(|k| v.get(k).and_then(|t| t.as_str()).map(String::from))
            .unwrap_or(text),
        Err(_) => text,
    })
}

fn clip(s: &str) -> String {
    let s = s.trim();
    match s.char_indices().nth(300) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_string(),
    }
}

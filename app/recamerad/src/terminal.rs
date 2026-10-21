//! The page's terminal (stage A8c): a WebSocket at /api/terminal joined to a
//! pseudo-terminal running `login`, which asks for the user and the password
//! as the console and ssh do (the Seeed page's terminal did too: being
//! logged in to the page is not enough for a shell). After it, the account's
//! shell, motd included.
//!
//! On the socket: binary messages are bytes, both ways (keys in, screen out);
//! a text message from the page is a JSON command, `{"resize":[cols,rows]}`.
//! Closing the page hangs the shell up.

use bytes::Bytes;
use futures_util::{SinkExt, StreamExt};
use hyper::body::Incoming;
use hyper::header::{self, HeaderValue};
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::io::unix::AsyncFd;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::handshake::derive_accept_key;
use tokio_tungstenite::tungstenite::protocol::Role;
use tokio_tungstenite::tungstenite::Message;

/// terminals open at once
const MAX_OPEN: usize = 4;
static OPEN: AtomicUsize = AtomicUsize::new(0);

/// The WebSocket handshake; the terminal runs in its own task after the 101.
pub fn upgrade(mut req: Request<Incoming>, user: String) -> Result<Response<Bytes>, (StatusCode, String)> {
    let h = req.headers();
    let is_ws = h.get(header::UPGRADE).and_then(|v| v.to_str().ok()).is_some_and(|v| v.eq_ignore_ascii_case("websocket"));
    let Some(key) = h.get("sec-websocket-key").filter(|_| is_ws) else {
        return Err((StatusCode::BAD_REQUEST, "a WebSocket request".into()));
    };
    // another site's page cannot open a shell (the session cookie is
    // SameSite=Strict as well)
    if let Some(origin) = h.get(header::ORIGIN).and_then(|o| o.to_str().ok()) {
        let host = h.get(header::HOST).and_then(|o| o.to_str().ok()).unwrap_or("");
        if origin.split_once("://").map(|(_, o)| o) != Some(host) {
            return Err((StatusCode::FORBIDDEN, "origin".into()));
        }
    }
    if OPEN.load(Ordering::Relaxed) >= MAX_OPEN {
        return Err((StatusCode::SERVICE_UNAVAILABLE, format!("{MAX_OPEN} terminals open already")));
    }
    let accept = derive_accept_key(key.as_bytes());
    tokio::spawn(async move {
        match hyper::upgrade::on(&mut req).await {
            Ok(up) => {
                let ws = WebSocketStream::from_raw_socket(TokioIo::new(up), Role::Server, None).await;
                OPEN.fetch_add(1, Ordering::Relaxed);
                crate::info!("terminal: opened from the page session of {user}");
                if let Err(e) = session(ws).await {
                    crate::warn!("terminal: {e}");
                }
                crate::info!("terminal: closed ({user})");
                OPEN.fetch_sub(1, Ordering::Relaxed);
            }
            Err(e) => crate::warn!("terminal: upgrade: {e}"),
        }
    });
    let mut r = Response::new(Bytes::new());
    *r.status_mut() = StatusCode::SWITCHING_PROTOCOLS;
    let hs = r.headers_mut();
    hs.insert(header::UPGRADE, HeaderValue::from_static("websocket"));
    hs.insert(header::CONNECTION, HeaderValue::from_static("Upgrade"));
    if let Ok(v) = HeaderValue::from_str(&accept) {
        hs.insert("sec-websocket-accept", v);
    }
    Ok(r)
}

fn set_size(fd: &OwnedFd, cols: u16, rows: u16) {
    let ws = libc::winsize { ws_row: rows, ws_col: cols, ws_xpixel: 0, ws_ypixel: 0 };
    // SAFETY: TIOCSWINSZ reads one winsize from a valid pointer
    unsafe { libc::ioctl(fd.as_raw_fd(), libc::TIOCSWINSZ, &ws) };
}

async fn session<S>(mut ws: WebSocketStream<S>) -> Result<(), String>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    // the pseudo-terminal
    let (mut m, mut s) = (-1, -1);
    // SAFETY: openpty fills two descriptors, owned right below
    if unsafe { libc::openpty(&mut m, &mut s, std::ptr::null_mut(), std::ptr::null(), std::ptr::null()) } != 0 {
        return Err(format!("openpty: {}", std::io::Error::last_os_error()));
    }
    // SAFETY: fresh descriptors from openpty, nobody else owns them
    let (master, slave) = unsafe { (OwnedFd::from_raw_fd(m), OwnedFd::from_raw_fd(s)) };
    // SAFETY: fcntl on a valid descriptor
    unsafe {
        let fl = libc::fcntl(m, libc::F_GETFL);
        libc::fcntl(m, libc::F_SETFL, fl | libc::O_NONBLOCK);
        libc::fcntl(m, libc::F_SETFD, libc::FD_CLOEXEC);
    }
    set_size(&master, 80, 24);

    // login asks, then sets the account's shell, environment and groups
    let mut cmd = tokio::process::Command::new("/bin/login");
    cmd.env_clear()
        .env("TERM", "xterm-256color")
        .stdin(slave.try_clone().map_err(|e| e.to_string())?)
        .stdout(slave.try_clone().map_err(|e| e.to_string())?)
        .stderr(slave)
        .kill_on_drop(true);
    // SAFETY: only async-signal-safe calls between fork and exec
    unsafe {
        cmd.pre_exec(|| {
            // a session of its own with the terminal as the controlling one
            if libc::setsid() < 0 || libc::ioctl(0, libc::TIOCSCTTY, 0) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = cmd.spawn().map_err(|e| format!("login: {e}"))?;
    let pid = child.id();
    // SAFETY: an OwnedFd keeps its descriptor open and unchanged while the
    // AsyncFd owns it
    let pty = unsafe { AsyncFd::register(master) }.map_err(|e| e.to_string())?;

    let mut buf = vec![0u8; 8192];
    let r = loop {
        tokio::select! {
            msg = ws.next() => match msg {
                Some(Ok(Message::Binary(b))) => {
                    if let Err(e) = write_all(&pty, &b).await {
                        break Err(e);
                    }
                }
                Some(Ok(Message::Text(t))) => {
                    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&t)
                        && let Some([c, r]) = v.get("resize").and_then(|r| r.as_array()).map(|a| a.as_slice())
                        && let (Some(c), Some(r)) = (c.as_u64(), r.as_u64())
                    {
                        set_size(pty.get_ref(), c.clamp(2, 500) as u16, r.clamp(2, 300) as u16);
                    }
                }
                Some(Ok(Message::Close(_))) | None => break Ok(()),
                Some(Ok(_)) => {}
                Some(Err(e)) => break Err(e.to_string()),
            },
            n = read(&pty, &mut buf) => match n {
                // EIO: the shell is gone (logout, exit)
                Ok(0) | Err(_) => {
                    let _ = ws.send(Message::text("{\"exit\":true}")).await;
                    let _ = ws.close(None).await;
                    break Ok(());
                }
                Ok(n) => {
                    if let Err(e) = ws.send(Message::binary(buf[..n].to_vec())).await {
                        break Err(e.to_string());
                    }
                }
            },
        }
    };
    // hang up the whole session (the shell and what it started)
    if let Some(pid) = pid {
        // SAFETY: a signal to the process group the child leads
        unsafe { libc::kill(-(pid as i32), libc::SIGHUP) };
    }
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), child.wait()).await;
    r
}

async fn read(pty: &AsyncFd<OwnedFd>, buf: &mut [u8]) -> std::io::Result<usize> {
    loop {
        let mut g = pty.readable().await?;
        // SAFETY: read into a buffer of buf.len() bytes
        match g.try_io(|fd| {
            let n = unsafe { libc::read(fd.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) };
            if n < 0 { Err(std::io::Error::last_os_error()) } else { Ok(n as usize) }
        }) {
            Ok(r) => return r,
            Err(_would_block) => continue,
        }
    }
}

async fn write_all(pty: &AsyncFd<OwnedFd>, mut data: &[u8]) -> Result<(), String> {
    while !data.is_empty() {
        let mut g = pty.writable().await.map_err(|e| e.to_string())?;
        // SAFETY: write from a buffer of data.len() bytes
        match g.try_io(|fd| {
            let n = unsafe { libc::write(fd.as_raw_fd(), data.as_ptr().cast(), data.len()) };
            if n < 0 { Err(std::io::Error::last_os_error()) } else { Ok(n as usize) }
        }) {
            Ok(Ok(n)) => data = &data[n..],
            Ok(Err(e)) => return Err(e.to_string()),
            Err(_would_block) => continue,
        }
    }
    Ok(())
}

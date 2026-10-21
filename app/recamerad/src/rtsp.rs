//! The RTSP server (stage A6): `rtsp://<camera>/live`, H.264 Baseline from
//! the hardware encoder, RTP over the RTSP TCP connection (interleaved) or
//! over UDP. The protocol pieces are in recamera-rtsp; this is the I/O.
//!
//! Each connection is two tasks: one reads the requests and answers them,
//! one writes (answers and, interleaved, the RTP packets). PLAY starts a
//! sender that waits for an IDR (asked for when the viewer joins) and
//! follows the broadcast of access units; a client that falls behind skips
//! to the next IDR. The encoder runs while some client plays (H264Viewer).

use crate::live::{H264Viewer, Live};
use crate::web::check_password;
use bytes::{BufMut, Bytes, BytesMut};
use recamera_rtsp::{Packetizer, Request, Transport};
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;

const SESSION_TIMEOUT_S: u64 = 60;
const MAX_REQUEST: usize = 16 * 1024;

/// Serve until the task is dropped.
pub async fn serve(live: Arc<Live>, port: u16, auth: bool) {
    let listener = match TcpListener::bind(("0.0.0.0", port)).await {
        Ok(l) => l,
        Err(e) => {
            crate::error!("rtsp: port {port}: {e}");
            return;
        }
    };
    crate::info!("rtsp on port {port} (rtsp://<camera>/live)");
    loop {
        let Ok((stream, peer)) = listener.accept().await else { continue };
        let live = live.clone();
        tokio::spawn(async move {
            let _ = stream.set_nodelay(true);
            connection(live, stream, peer, auth).await;
            crate::debug!("rtsp: {peer} closed");
        });
    }
}

struct Session {
    id: u64,
    transport: Option<Transport>,
    udp: Option<Arc<UdpSocket>>,
    sender: Option<JoinHandle<()>>,
}

impl Drop for Session {
    fn drop(&mut self) {
        if let Some(s) = self.sender.take() {
            s.abort();
        }
    }
}

async fn connection(live: Arc<Live>, stream: TcpStream, peer: SocketAddr, auth: bool) {
    let local = stream.local_addr().map(|a| a.ip().to_string()).unwrap_or_else(|_| "0.0.0.0".into());
    let (mut rd, mut wr) = stream.into_split();
    let (tx, mut rx) = mpsc::channel::<Bytes>(64);
    let writer = tokio::spawn(async move {
        while let Some(b) = rx.recv().await {
            if wr.write_all(&b).await.is_err() {
                return;
            }
        }
    });
    let mut s = Session { id: random_u64(), transport: None, udp: None, sender: None };
    let mut buf = BytesMut::with_capacity(4096);
    loop {
        // a client keeps the session alive with GET_PARAMETER/OPTIONS
        let r = tokio::time::timeout(Duration::from_secs(SESSION_TIMEOUT_S + 10), rd.read_buf(&mut buf)).await;
        match r {
            Ok(Ok(n)) if n > 0 => {}
            _ => break,
        }
        loop {
            // RTCP from the client, interleaved: '$', channel, length
            if buf.first() == Some(&b'$') {
                if buf.len() < 4 {
                    break;
                }
                let n = 4 + u16::from_be_bytes([buf[2], buf[3]]) as usize;
                if buf.len() < n {
                    break;
                }
                let _ = buf.split_to(n);
                continue;
            }
            match Request::parse(&buf) {
                None => break,
                Some(Err(e)) => {
                    crate::debug!("rtsp: {peer}: {e}");
                    writer.abort();
                    return;
                }
                Some(Ok((req, used))) => {
                    let _ = buf.split_to(used);
                    let resp = handle(&live, &mut s, &req, peer.ip(), &local, auth, &tx).await;
                    if tx.send(resp).await.is_err() {
                        return;
                    }
                    if req.method == "TEARDOWN" {
                        drop(s);
                        drop(tx);
                        let _ = writer.await;
                        return;
                    }
                }
            }
        }
        if buf.len() > MAX_REQUEST {
            break;
        }
    }
    drop(s);
    writer.abort();
}

fn random_u64() -> u64 {
    use std::io::Read;
    let mut b = [0u8; 8];
    let _ = std::fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut b));
    u64::from_le_bytes(b) & 0x7fff_ffff_ffff_ffff
}

fn response(req: &Request, status: &str, headers: &[(&str, String)], body: &str) -> Bytes {
    let mut r = format!("RTSP/1.0 {status}\r\nCSeq: {}\r\nServer: recamerad\r\n", req.header("CSeq").unwrap_or("0"));
    for (k, v) in headers {
        r.push_str(&format!("{k}: {v}\r\n"));
    }
    if !body.is_empty() {
        r.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    r.push_str("\r\n");
    r.push_str(body);
    Bytes::from(r)
}

async fn handle(live: &Arc<Live>, s: &mut Session, req: &Request, peer: IpAddr, local: &str, auth: bool,
                tx: &mpsc::Sender<Bytes>) -> Bytes {
    if req.method == "OPTIONS" {
        let methods = "OPTIONS, DESCRIBE, SETUP, PLAY, TEARDOWN, GET_PARAMETER, SET_PARAMETER";
        return response(req, "200 OK", &[("Public", methods.into())], "");
    }
    if auth && !req.basic_auth().is_some_and(|(u, p)| check_password(&u, &p)) {
        if req.header("Authorization").is_some() {
            crate::warn!("rtsp: failed login from {peer}");
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
        return response(req, "401 Unauthorized", &[("WWW-Authenticate", "Basic realm=\"reCamera\"".into())], "");
    }
    let session = format!("{};timeout={SESSION_TIMEOUT_S}", s.id);
    match req.method.as_str() {
        "DESCRIBE" => {
            let Some((sps, pps)) = parameters(live).await else {
                return response(req, "503 Service Unavailable", &[], "");
            };
            let sdp = recamera_rtsp::sdp(local, s.id, &sps, &pps);
            let base = format!("{}/", req.uri.trim_end_matches('/'));
            response(req, "200 OK", &[("Content-Type", "application/sdp".into()), ("Content-Base", base)], &sdp)
        }
        "SETUP" => {
            let Some(t) = req.header("Transport").and_then(Transport::parse) else {
                return response(req, "461 Unsupported Transport", &[], "");
            };
            let reply = match t {
                Transport::Interleaved(ch) => format!("RTP/AVP/TCP;unicast;interleaved={}-{}", ch, ch + 1),
                Transport::Udp(a, b) => {
                    let sock = match UdpSocket::bind(("0.0.0.0", 0)).await {
                        Ok(u) => u,
                        Err(_) => return response(req, "500 Internal Server Error", &[], ""),
                    };
                    let sp = sock.local_addr().map(|a| a.port()).unwrap_or(0);
                    s.udp = Some(Arc::new(sock));
                    format!("RTP/AVP;unicast;client_port={a}-{b};server_port={sp}-{}", sp + 1)
                }
            };
            s.transport = Some(t);
            response(req, "200 OK", &[("Transport", reply), ("Session", session)], "")
        }
        "PLAY" => {
            let Some(t) = s.transport else {
                return response(req, "455 Method Not Valid in This State", &[], "");
            };
            if s.sender.is_none() {
                let out = match t {
                    Transport::Interleaved(ch) => Out::Tcp(tx.clone(), ch),
                    Transport::Udp(port, _) => match &s.udp {
                        Some(u) => Out::Udp(u.clone(), SocketAddr::new(peer, port)),
                        None => return response(req, "455 Method Not Valid in This State", &[], ""),
                    },
                };
                crate::info!("rtsp: {peer} playing ({})", if matches!(t, Transport::Udp(..)) { "UDP" } else { "TCP" });
                s.sender = Some(tokio::spawn(send(live.h264_viewer(), live.h264.subscribe(), out, s.id as u32)));
            }
            response(req, "200 OK", &[("Session", session), ("Range", "npt=0.000-".into())], "")
        }
        "TEARDOWN" => {
            if let Some(h) = s.sender.take() {
                h.abort();
            }
            response(req, "200 OK", &[("Session", session)], "")
        }
        "GET_PARAMETER" | "SET_PARAMETER" => response(req, "200 OK", &[("Session", session)], ""),
        _ => response(req, "501 Not Implemented", &[], ""),
    }
}

/// SPS/PPS for the SDP: the last ones, or wait for the encoder to start.
async fn parameters(live: &Arc<Live>) -> Option<(Bytes, Bytes)> {
    let p = live.h264_params.lock().ok().and_then(|p| p.clone());
    if p.is_some() {
        return p;
    }
    let _viewer = live.h264_viewer();
    for _ in 0..30 {
        tokio::time::sleep(Duration::from_millis(100)).await;
        if let Some(p) = live.h264_params.lock().ok().and_then(|p| p.clone()) {
            return Some(p);
        }
    }
    None
}

enum Out {
    Tcp(mpsc::Sender<Bytes>, u8),
    Udp(Arc<UdpSocket>, SocketAddr),
}

/// RTP of every access unit from the next IDR on, until the client leaves.
async fn send(_viewer: H264Viewer, mut rx: broadcast::Receiver<Arc<crate::live::Au>>, out: Out, ssrc: u32) {
    let mut pk = Packetizer::new(ssrc, ssrc as u16);
    let mut synced = false;
    loop {
        let au = match rx.recv().await {
            Ok(au) => au,
            Err(broadcast::error::RecvError::Lagged(_)) => {
                synced = false; // frames lost: restart at an IDR
                continue;
            }
            Err(broadcast::error::RecvError::Closed) => return,
        };
        if !synced && !au.key {
            continue;
        }
        synced = true;
        let ts = (au.pts_us.wrapping_mul(9) / 100) as u32;
        let packets = pk.packetize(&au.data, ts);
        match &out {
            Out::Tcp(tx, ch) => {
                // one write per access unit: '$', channel, length, packet
                let mut b = BytesMut::with_capacity(au.data.len() + packets.len() * 16);
                for p in &packets {
                    b.put_u8(b'$');
                    b.put_u8(*ch);
                    b.put_u16(p.len() as u16);
                    b.put_slice(p);
                }
                if tx.send(b.freeze()).await.is_err() {
                    return;
                }
            }
            Out::Udp(sock, to) => {
                for p in &packets {
                    if sock.send_to(p, to).await.is_err() {
                        return;
                    }
                }
            }
        }
    }
}

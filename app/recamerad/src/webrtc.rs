//! WebRTC for the page (stage A7): WHEP (`POST /whep` with the browser's SDP
//! offer, `DELETE /whep/<id>`), str0m (sans-IO) on the network thread, the
//! same H.264 access units as RTSP (no encoding per viewer).
//!
//! One task and one UDP socket per viewer. The camera offers one host
//! candidate, the address the browser reached the page on; the browser's own
//! candidates (often mDNS names) are learnt from its STUN checks. A new
//! viewer, a lost frame or a browser keyframe request asks the encoder for
//! an IDR, and the viewer starts or restarts there.

use crate::live::{Au, Live};
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex, Once};
use std::time::Instant;
use str0m::change::SdpOffer;
use str0m::format::Codec;
use str0m::media::{MediaKind, MediaTime, Mid, Pt};
use str0m::net::{Protocol, Receive};
use str0m::{Candidate, Event, IceConnectionState, Input, Output, Rtc, RtcConfig};
use tokio::net::UdpSocket;
use tokio::sync::broadcast;
use tokio::task::JoinHandle;

/// browsers watching at once (each one is a DTLS/SRTP session)
const MAX_VIEWERS: usize = 4;

#[derive(Default)]
pub struct Sessions {
    next: Mutex<u64>,
    running: Mutex<HashMap<u64, JoinHandle<()>>>,
}

impl Sessions {
    /// Answer an offer and start the session; returns (id, answer SDP).
    pub async fn open(self: &Arc<Self>, live: &Arc<Live>, offer: &str, local_ip: IpAddr)
        -> Result<(u64, String), String> {
        static CRYPTO: Once = Once::new();
        CRYPTO.call_once(|| str0m::crypto::from_feature_flags().install_process_default());

        if let Ok(mut r) = self.running.lock() {
            r.retain(|_, h| !h.is_finished());
            if r.len() >= MAX_VIEWERS {
                return Err(format!("{MAX_VIEWERS} viewers already"));
            }
        }
        let offer = SdpOffer::from_sdp_string(offer).map_err(|e| format!("offer: {e}"))?;
        let sock = UdpSocket::bind((local_ip, 0)).await.map_err(|e| format!("udp: {e}"))?;
        let local = sock.local_addr().map_err(|e| e.to_string())?;
        let mut rtc = RtcConfig::new().clear_codecs().enable_h264(true).build(Instant::now());
        let cand = Candidate::host(local, "udp").map_err(|e| format!("candidate: {e}"))?;
        rtc.add_local_candidate(cand);
        let answer = rtc.sdp_api().accept_offer(offer).map_err(|e| format!("answer: {e}"))?;

        let id = {
            let mut n = self.next.lock().map_err(|_| "lock")?;
            *n += 1;
            *n
        };
        let me = self.clone();
        let task = tokio::spawn(run(rtc, sock, local, live.clone(), id, me));
        if let Ok(mut r) = self.running.lock() {
            r.insert(id, task);
        }
        Ok((id, answer.to_sdp_string()))
    }

    pub fn close(&self, id: u64) -> bool {
        match self.running.lock().ok().and_then(|mut r| r.remove(&id)) {
            Some(h) => {
                h.abort();
                true
            }
            None => false,
        }
    }
}

/// The H.264 payload type the browser accepted: packetization mode 1,
/// Baseline/Constrained Baseline (what the encoder makes) first.
fn h264_pt(rtc: &mut Rtc, mid: Mid) -> Option<Pt> {
    let w = rtc.writer(mid)?;
    let mut best: Option<(u8, Pt)> = None;
    for p in w.payload_params() {
        let s = p.spec();
        if s.codec != Codec::H264 {
            continue;
        }
        let mode1 = s.format.packetization_mode == Some(1);
        let baseline = s.format.profile_level_id.is_some_and(|v| (v >> 16) == 0x42);
        let rank = mode1 as u8 * 2 + baseline as u8;
        if best.is_none_or(|(r, _)| rank > r) {
            best = Some((rank, p.pt()));
        }
    }
    best.map(|(_, pt)| pt)
}

async fn run(mut rtc: Rtc, sock: UdpSocket, local: SocketAddr, live: Arc<Live>, id: u64, sessions: Arc<Sessions>) {
    let _viewer = live.h264_viewer();
    let mut frames: broadcast::Receiver<Arc<Au>> = live.h264.subscribe();
    let mut video: Option<(Mid, Option<Pt>)> = None;
    let mut synced = false;
    let mut buf = vec![0u8; 2000];
    let mut peer: Option<SocketAddr> = None;
    let start = Instant::now();

    'session: loop {
        // everything str0m wants to send, then its next deadline
        let deadline = loop {
            match rtc.poll_output() {
                Ok(Output::Timeout(t)) => break t,
                Ok(Output::Transmit(t)) => {
                    let _ = sock.send_to(&t.contents, t.destination).await;
                }
                Ok(Output::Event(e)) => match e {
                    Event::MediaAdded(m) if m.kind == MediaKind::Video => video = Some((m.mid, None)),
                    Event::Connected => {
                        crate::info!("webrtc {id}: connected{}", peer.map(|p| format!(" ({})", p.ip())).unwrap_or_default());
                        live.idr_wanted.store(true, std::sync::atomic::Ordering::Relaxed);
                    }
                    Event::KeyframeRequest(_) => {
                        live.idr_wanted.store(true, std::sync::atomic::Ordering::Relaxed);
                    }
                    Event::IceConnectionStateChange(IceConnectionState::Disconnected) => break 'session,
                    _ => {}
                },
                Err(e) => {
                    crate::debug!("webrtc {id}: {e}");
                    break 'session;
                }
            }
        };
        if !rtc.is_alive() {
            break;
        }
        // a browser that never connects (wrong network, closed tab)
        if !rtc.is_connected() && start.elapsed().as_secs() > 30 {
            crate::warn!("webrtc {id}: no connection in 30 s");
            break;
        }

        let wait = deadline.saturating_duration_since(Instant::now());
        let input = tokio::select! {
            r = sock.recv_from(&mut buf) => match r {
                Ok((n, source)) => {
                    peer.get_or_insert(source);
                    match (&buf[..n]).try_into() {
                        Ok(contents) => Input::Receive(Instant::now(),
                            Receive { proto: Protocol::Udp, source, destination: local, contents }),
                        Err(_) => continue,
                    }
                }
                Err(_) => continue,
            },
            r = frames.recv() => {
                match r {
                    Ok(au) => {
                        let Some((mid, pt)) = video.as_mut() else { continue };
                        if !rtc.is_connected() || (!synced && !au.key) {
                            continue;
                        }
                        if pt.is_none() {
                            *pt = h264_pt(&mut rtc, *mid);
                        }
                        let (Some(p), Some(w)) = (*pt, rtc.writer(*mid)) else { continue };
                        let t = MediaTime::from_90khz(au.pts_us.wrapping_mul(9) / 100);
                        if let Err(e) = w.write(p, Instant::now(), t, Arc::<[u8]>::from(&au.data[..])) {
                            crate::debug!("webrtc {id}: write: {e}");
                        }
                        synced = true;
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        synced = false;
                        live.idr_wanted.store(true, std::sync::atomic::Ordering::Relaxed);
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
                continue;
            }
            _ = tokio::time::sleep(wait) => Input::Timeout(Instant::now()),
        };
        if let Err(e) = rtc.handle_input(input) {
            crate::debug!("webrtc {id}: {e}");
            break;
        }
    }
    crate::info!("webrtc {id}: closed");
    if let Ok(mut r) = sessions.running.lock() {
        r.remove(&id);
    }
}

//! The protocol pieces of the reCamera RTSP server, without I/O (tested on
//! the host with `cargo test -p recamera-rtsp`):
//!
//! - [`nals`]: the NAL units of an H.264 Annex B access unit
//! - [`Packetizer`]: RTP packets of an access unit (RFC 6184: single NAL
//!   unit packets and FU-A fragments, marker on the last packet)
//! - [`sdp`]: the session description (sprop-parameter-sets from SPS/PPS)
//! - [`Request`]: an RTSP request; [`Transport`]: its Transport header
//! - [`base64`], [`base64_decode`]: for the SDP and Basic authentication

/// The NAL units (without start codes) of an Annex B byte stream.
pub fn nals(au: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 <= au.len() {
        if au[i] == 0 && au[i + 1] == 0 && au[i + 2] == 1 {
            starts.push(i + 3);
            i += 3;
        } else {
            i += 1;
        }
    }
    let mut out = Vec::with_capacity(starts.len());
    for (k, &s) in starts.iter().enumerate() {
        let mut e = if k + 1 < starts.len() { starts[k + 1] - 3 } else { au.len() };
        // a 4-byte start code leaves a zero before the next 00 00 01
        while e > s && au[e - 1] == 0 && k + 1 < starts.len() {
            e -= 1;
        }
        if e > s {
            out.push(&au[s..e]);
        }
    }
    out
}

pub fn nal_type(nal: &[u8]) -> u8 {
    nal.first().map(|b| b & 0x1f).unwrap_or(0)
}

pub const NAL_IDR: u8 = 5;
pub const NAL_SPS: u8 = 7;
pub const NAL_PPS: u8 = 8;

/// RTP packets of H.264 access units, payload type 96, clock 90 kHz.
pub struct Packetizer {
    pub ssrc: u32,
    pub seq: u16,
    /// RTP payload size limit (fits an Ethernet/Wi-Fi MTU with IP+UDP+RTP)
    pub max_payload: usize,
}

impl Packetizer {
    pub fn new(ssrc: u32, seq: u16) -> Packetizer {
        Packetizer { ssrc, seq, max_payload: 1400 }
    }

    fn header(&mut self, out: &mut Vec<u8>, marker: bool, ts: u32) {
        out.push(0x80); // V=2
        out.push(if marker { 0x80 | 96 } else { 96 });
        out.extend_from_slice(&self.seq.to_be_bytes());
        out.extend_from_slice(&ts.to_be_bytes());
        out.extend_from_slice(&self.ssrc.to_be_bytes());
        self.seq = self.seq.wrapping_add(1);
    }

    /// The RTP packets of one access unit (Annex B), timestamp `ts`.
    pub fn packetize(&mut self, au: &[u8], ts: u32) -> Vec<Vec<u8>> {
        let units = nals(au);
        let mut packets = Vec::new();
        for (k, nal) in units.iter().enumerate() {
            let last_nal = k + 1 == units.len();
            if nal.len() <= self.max_payload {
                let mut p = Vec::with_capacity(12 + nal.len());
                self.header(&mut p, last_nal, ts);
                p.extend_from_slice(nal);
                packets.push(p);
                continue;
            }
            // FU-A: indicator (F, NRI, type 28), header (S, E, type)
            let (ind, typ) = ((nal[0] & 0xe0) | 28, nal[0] & 0x1f);
            let body = &nal[1..];
            let chunk = self.max_payload - 2;
            let n = body.len().div_ceil(chunk);
            for (i, c) in body.chunks(chunk).enumerate() {
                let (start, end) = (i == 0, i + 1 == n);
                let mut p = Vec::with_capacity(14 + c.len());
                self.header(&mut p, last_nal && end, ts);
                p.push(ind);
                p.push(typ | if start { 0x80 } else { 0 } | if end { 0x40 } else { 0 });
                p.extend_from_slice(c);
                packets.push(p);
            }
        }
        packets
    }
}

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn base64(data: &[u8]) -> String {
    let mut s = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= c.len() {
                s.push(B64[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                s.push('=');
            }
        }
    }
    s
}

pub fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let (mut acc, mut bits) = (0u32, 0);
    for ch in s.trim().bytes() {
        if ch == b'=' {
            break;
        }
        let v = B64.iter().position(|&b| b == ch)? as u32;
        acc = acc << 6 | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

/// The SDP of the H.264 stream (one video track, "track1").
pub fn sdp(server_ip: &str, session_id: u64, sps: &[u8], pps: &[u8]) -> String {
    let profile = if sps.len() >= 4 {
        format!(";profile-level-id={:02x}{:02x}{:02x}", sps[1], sps[2], sps[3])
    } else {
        String::new()
    };
    format!(
        "v=0\r\no=- {session_id} 1 IN IP4 {server_ip}\r\ns=reCamera\r\nc=IN IP4 0.0.0.0\r\nt=0 0\r\n\
         a=control:*\r\na=range:npt=0-\r\nm=video 0 RTP/AVP 96\r\na=rtpmap:96 H264/90000\r\n\
         a=fmtp:96 packetization-mode=1{profile};sprop-parameter-sets={},{}\r\na=control:track1\r\n",
        base64(sps),
        base64(pps)
    )
}

/// An RTSP request (request line + headers; the clients send no body we use).
#[derive(Debug)]
pub struct Request {
    pub method: String,
    pub uri: String,
    pub headers: Vec<(String, String)>,
}

impl Request {
    /// Parse one request from `buf` if it holds a whole header block;
    /// returns the request and the bytes it used.
    pub fn parse(buf: &[u8]) -> Option<Result<(Request, usize), String>> {
        let end = buf.windows(4).position(|w| w == b"\r\n\r\n")? + 4;
        let text = match std::str::from_utf8(&buf[..end]) {
            Ok(t) => t,
            Err(_) => return Some(Err("not UTF-8".into())),
        };
        let mut lines = text.split("\r\n");
        let mut first = lines.next().unwrap_or("").split_whitespace();
        let (Some(method), Some(uri), Some(ver)) = (first.next(), first.next(), first.next()) else {
            return Some(Err("bad request line".into()));
        };
        if !ver.starts_with("RTSP/") {
            return Some(Err(format!("not RTSP: {ver}")));
        }
        let headers = lines
            .filter_map(|l| l.split_once(':').map(|(k, v)| (k.trim().to_string(), v.trim().to_string())))
            .collect::<Vec<_>>();
        // a body (Content-Length) is skipped
        let body = headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
            .and_then(|(_, v)| v.parse::<usize>().ok())
            .unwrap_or(0);
        if buf.len() < end + body {
            return None;
        }
        Some(Ok((Request { method: method.to_string(), uri: uri.to_string(), headers }, end + body)))
    }

    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }

    /// user and password of a Basic Authorization header
    pub fn basic_auth(&self) -> Option<(String, String)> {
        let v = self.header("Authorization")?;
        let b = v.strip_prefix("Basic ").or_else(|| v.strip_prefix("basic "))?;
        let d = String::from_utf8(base64_decode(b)?).ok()?;
        let (u, p) = d.split_once(':')?;
        Some((u.to_string(), p.to_string()))
    }
}

/// How the client wants the RTP packets.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Transport {
    /// in the RTSP TCP connection, '$' framed, on this channel
    Interleaved(u8),
    /// UDP to these client ports (RTP, RTCP)
    Udp(u16, u16),
}

impl Transport {
    pub fn parse(h: &str) -> Option<Transport> {
        // a client may offer several, comma separated: take the first we know
        for t in h.split(',') {
            let params: Vec<&str> = t.split(';').map(str::trim).collect();
            let tcp = params.first().is_some_and(|p| p.eq_ignore_ascii_case("RTP/AVP/TCP"));
            let udp = params.first().is_some_and(|p| p.eq_ignore_ascii_case("RTP/AVP")
                || p.eq_ignore_ascii_case("RTP/AVP/UDP"));
            let range = |key: &str| {
                params.iter().find_map(|p| p.strip_prefix(key)).and_then(|r| {
                    let (a, b) = r.split_once('-').unwrap_or((r, ""));
                    Some((a.parse::<u16>().ok()?, b.parse::<u16>().ok()))
                })
            };
            if tcp {
                let ch = range("interleaved=").map(|(a, _)| a as u8).unwrap_or(0);
                return Some(Transport::Interleaved(ch));
            }
            if udp && let Some((a, b)) = range("client_port=") {
                return Some(Transport::Udp(a, b.unwrap_or(a + 1)));
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn annexb_split() {
        let au = [0, 0, 0, 1, 0x67, 1, 2, 0, 0, 1, 0x68, 3, 0, 0, 0, 1, 0x65, 4, 5, 6];
        let n = nals(&au);
        assert_eq!(n, vec![&[0x67, 1, 2][..], &[0x68, 3][..], &[0x65, 4, 5, 6][..]]);
        assert_eq!(nal_type(n[2]), NAL_IDR);
    }

    #[test]
    fn small_nal_is_one_packet() {
        let mut p = Packetizer::new(0x11223344, 7);
        let pk = p.packetize(&[0, 0, 1, 0x41, 9, 9], 1000);
        assert_eq!(pk.len(), 1);
        assert_eq!(pk[0][1], 0x80 | 96); // marker
        assert_eq!(&pk[0][2..4], &7u16.to_be_bytes());
        assert_eq!(&pk[0][12..], &[0x41, 9, 9]);
        assert_eq!(p.seq, 8);
    }

    #[test]
    fn big_nal_is_fu_a() {
        let mut nal = vec![0x65u8];
        nal.extend((0..3000u32).map(|i| i as u8));
        let mut au = vec![0, 0, 0, 1];
        au.extend(&nal);
        let mut p = Packetizer::new(1, 0);
        let pk = p.packetize(&au, 5);
        assert_eq!(pk.len(), 3); // 2999 bytes in chunks of 1398
        assert_eq!(pk[0][12], 0x60 | 28); // NRI of 0x65, FU-A
        assert_eq!(pk[0][13], 0x80 | 5); // start, type 5
        assert_eq!(pk[1][13], 5);
        assert_eq!(pk[2][13], 0x40 | 5); // end
        assert_eq!(pk[2][1], 0x80 | 96);
        assert_eq!(pk[0][1], 96);
        // the payloads put back together are the NAL
        let mut back = vec![0x65u8];
        for k in &pk {
            back.extend(&k[14..]);
        }
        assert_eq!(back, nal);
    }

    #[test]
    fn base64_round_trip() {
        for s in ["", "a", "ab", "abc", "recamera:recamera"] {
            assert_eq!(base64_decode(&base64(s.as_bytes())).unwrap(), s.as_bytes());
        }
        assert_eq!(base64(b"recamera:recamera"), "cmVjYW1lcmE6cmVjYW1lcmE=");
    }

    #[test]
    fn request_and_transport() {
        let raw = b"SETUP rtsp://cam/live/track1 RTSP/1.0\r\nCSeq: 3\r\n\
                    Transport: RTP/AVP;unicast;client_port=5000-5001\r\n\
                    Authorization: Basic cmVjYW1lcmE6cmVjYW1lcmE=\r\n\r\nrest";
        let (r, n) = Request::parse(raw).unwrap().unwrap();
        assert_eq!(n, raw.len() - 4);
        assert_eq!(r.method, "SETUP");
        assert_eq!(r.header("cseq"), Some("3"));
        assert_eq!(r.basic_auth(), Some(("recamera".into(), "recamera".into())));
        assert_eq!(Transport::parse(r.header("Transport").unwrap()), Some(Transport::Udp(5000, 5001)));
        assert_eq!(Transport::parse("RTP/AVP/TCP;unicast;interleaved=0-1"), Some(Transport::Interleaved(0)));
        assert!(Request::parse(b"OPTIONS * RTSP/1.0\r\nCSeq: 1\r\n").is_none());
    }
}

//! Network section of the page (stage A10): the Wi-Fi client through the
//! wpa_supplicant control socket (its text protocol, no extra crate), and
//! the addresses of every interface.
//!
//! wpa_supplicant runs with -c /etc/wpa_supplicant.conf; SAVE_CONFIG writes
//! the networks there, and they are copied to /data/recamera (which
//! /etc/network/interfaces copies back at boot), so they outlive an A/B
//! update.

use std::path::PathBuf;
use std::time::Duration;
use tokio::net::UnixDatagram;

const CTRL: &str = "/var/run/wpa_supplicant/wlan0";
const CONF: &str = "/etc/wpa_supplicant.conf";
const KEPT: &str = "/data/recamera/wpa_supplicant.conf";

/// SAVE_CONFIG, then keep the file in /data (atomic copy)
async fn save() -> Result<(), String> {
    ok(request("SAVE_CONFIG").await?)?;
    let data = std::fs::read(CONF).map_err(|e| format!("{CONF}: {e}"))?;
    let tmp = format!("{KEPT}.new");
    std::fs::write(&tmp, data).map_err(|e| format!("{tmp}: {e}"))?;
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    if let Ok(f) = std::fs::File::open(&tmp) {
        let _ = f.sync_all();
    }
    std::fs::rename(&tmp, KEPT).map_err(|e| format!("{KEPT}: {e}"))
}
const TIMEOUT: Duration = Duration::from_secs(3);

/// One request to wpa_supplicant; the reply as text.
pub async fn request(cmd: &str) -> Result<String, String> {
    // the reply comes to our own bound socket
    static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let local = PathBuf::from(format!("/run/recamerad/wpa_ctrl_{}_{n}", std::process::id()));
    let _ = std::fs::create_dir_all("/run/recamerad");
    let _ = std::fs::remove_file(&local);
    let sock = UnixDatagram::bind(&local).map_err(|e| format!("wpa ctrl: {e}"))?;
    let r = async {
        sock.connect(CTRL).map_err(|e| format!("wpa_supplicant: {e}"))?;
        sock.send(cmd.as_bytes()).await.map_err(|e| e.to_string())?;
        let mut buf = vec![0u8; 8192];
        loop {
            let n = tokio::time::timeout(TIMEOUT, sock.recv(&mut buf))
                .await
                .map_err(|_| format!("wpa_supplicant: no answer to {}", cmd.split(' ').next().unwrap_or("")))?
                .map_err(|e| e.to_string())?;
            let text = String::from_utf8_lossy(&buf[..n]).into_owned();
            // unsolicited event messages start with '<' (priority)
            if !text.starts_with('<') {
                return Ok(text);
            }
        }
    }
    .await;
    let _ = std::fs::remove_file(&local);
    r
}

fn ok(reply: String) -> Result<(), String> {
    if reply.trim() == "OK" { Ok(()) } else { Err(format!("wpa_supplicant: {}", reply.trim())) }
}

/// key=value lines of STATUS / SIGNAL_POLL
fn kv(text: &str) -> serde_json::Map<String, serde_json::Value> {
    text.lines()
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (k.to_string(), serde_json::Value::from(v)))
        .collect()
}

/// The Wi-Fi state: connection, signal and the saved networks.
pub async fn wifi_status() -> serde_json::Value {
    let status = request("STATUS").await;
    let signal = request("SIGNAL_POLL").await.ok().map(|s| kv(&s));
    let saved: Vec<serde_json::Value> = request("LIST_NETWORKS")
        .await
        .map(|t| {
            t.lines()
                .skip(1)
                .filter_map(|l| {
                    let f: Vec<&str> = l.split('\t').collect();
                    (f.len() >= 2).then(|| {
                        serde_json::json!({
                            "id": f[0].parse::<u32>().unwrap_or(0),
                            "ssid": f[1],
                            "current": f.get(3).is_some_and(|x| x.contains("CURRENT")),
                            "disabled": f.get(3).is_some_and(|x| x.contains("DISABLED")),
                        })
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    match status {
        Ok(s) => serde_json::json!({ "status": kv(&s), "signal": signal, "saved": saved }),
        Err(e) => serde_json::json!({ "error": e, "saved": saved }),
    }
}

/// Scan and return the networks in range, the strongest of each SSID first.
pub async fn scan() -> Result<Vec<serde_json::Value>, String> {
    let r = request("SCAN").await?;
    if r.trim() != "OK" && !r.contains("FAIL-BUSY") {
        return Err(format!("scan: {}", r.trim()));
    }
    tokio::time::sleep(Duration::from_secs(4)).await;
    let t = request("SCAN_RESULTS").await?;
    let mut best: std::collections::HashMap<String, (i32, u32, String)> = std::collections::HashMap::new();
    for l in t.lines().skip(1) {
        let f: Vec<&str> = l.split('\t').collect();
        if f.len() < 5 || f[4].is_empty() {
            continue; // hidden network
        }
        let (freq, sig) = (f[1].parse::<u32>().unwrap_or(0), f[2].parse::<i32>().unwrap_or(-100));
        let e = best.entry(f[4].to_string()).or_insert((sig, freq, f[3].to_string()));
        if sig > e.0 {
            *e = (sig, freq, f[3].to_string());
        }
    }
    let mut v: Vec<_> = best.into_iter().collect();
    v.sort_by(|a, b| b.1.0.cmp(&a.1.0));
    Ok(v.into_iter()
        .map(|(ssid, (sig, freq, flags))| {
            let secure = flags.contains("WPA") || flags.contains("RSN") || flags.contains("WEP");
            serde_json::json!({ "ssid": ssid, "signal": sig, "freq": freq, "secure": secure, "flags": flags })
        })
        .collect())
}

/// The SSID as wpa_supplicant takes any bytes: hex, no quoting issues.
fn hex(s: &str) -> String {
    s.bytes().map(|b| format!("{b:02x}")).collect()
}

/// Connect to `ssid` (a new or a saved network) and save the configuration.
/// `psk` empty: an open network. The other saved networks stay, as fallback.
pub async fn connect(ssid: &str, psk: &str) -> Result<(), String> {
    if ssid.is_empty() || ssid.len() > 32 {
        return Err("SSID: 1 to 32 bytes".into());
    }
    if !psk.is_empty() && !(8..=63).contains(&psk.len()) {
        return Err("password: 8 to 63 characters".into());
    }
    if psk.contains('"') || psk.contains('\n') {
        return Err("password: no quote or newline".into());
    }
    // a saved network of the same SSID is replaced
    let list = request("LIST_NETWORKS").await?;
    for l in list.lines().skip(1) {
        let f: Vec<&str> = l.split('\t').collect();
        if f.len() >= 2 && f[1] == ssid {
            request(&format!("REMOVE_NETWORK {}", f[0])).await?;
        }
    }
    let id = request("ADD_NETWORK").await?.trim().to_string();
    if id.parse::<u32>().is_err() {
        return Err(format!("wpa_supplicant: {id}"));
    }
    ok(request(&format!("SET_NETWORK {id} ssid {}", hex(ssid))).await?)?;
    if psk.is_empty() {
        ok(request(&format!("SET_NETWORK {id} key_mgmt NONE")).await?)?;
    } else {
        ok(request(&format!("SET_NETWORK {id} psk \"{psk}\"")).await?)?;
    }
    ok(request(&format!("SELECT_NETWORK {id}")).await?)?;
    // SELECT_NETWORK disables the others: enable them again as fallback
    let _ = request("ENABLE_NETWORK all").await;
    save().await
}

pub async fn forget(id: u32) -> Result<(), String> {
    ok(request(&format!("REMOVE_NETWORK {id}")).await?)?;
    save().await
}

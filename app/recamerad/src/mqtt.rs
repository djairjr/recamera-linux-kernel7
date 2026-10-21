//! MQTT and Home Assistant (stage A9).
//!
//! Topics under `<base_topic>/<device id>` (the id from the Wi-Fi MAC):
//! - `status`: online/offline (retained; offline is the last will)
//! - `target`: ON/OFF, the presence of the target (retained)
//! - `event`: the target_arrived/target_left events (JSON)
//! - `telemetry`: temperature, load, memory, YOLO rate (JSON, retained)
//! - `snapshot`: a JPEG at every arrival
//! - `llm`: the last LLM answer (JSON: text or error, ms, label)
//! - `camera`: a JPEG every `camera_interval_s` (the Home Assistant camera
//!   entity; live video in Home Assistant is the RTSP stream, added there with
//!   the Generic Camera integration, which discovery cannot do)
//! - `detection` + `detection/set`, `led` + `led/set` + `led/brightness[/set]`:
//!   switches Home Assistant can turn (written to the configuration and
//!   applied as a reload, the same path as the page)
//!
//! Home Assistant discovery (`<prefix>/<component>/<id>/<object>/config`)
//! describes all of it, so the camera shows up as one device.

use crate::config::{self, Config};
use crate::events::Bus;
use crate::live::Live;
use rumqttc::{AsyncClient, Event, EventLoop, LastWill, MqttOptions, Packet, QoS, Transport};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{broadcast, mpsc, watch};

/// What MQTT needs to change the configuration (as the page does).
#[derive(Clone)]
pub struct Control {
    pub config_path: PathBuf,
    pub config: watch::Receiver<Config>,
    pub reload: mpsc::Sender<()>,
}

/// recamera_ + the last 3 bytes of the Wi-Fi MAC
pub fn device_id() -> String {
    let mac = std::fs::read_to_string("/sys/class/net/wlan0/address").unwrap_or_default();
    let tail: String = mac.trim().split(':').skip(3).collect();
    if tail.is_empty() { "recamera".into() } else { format!("recamera_{tail}") }
}

fn options(m: &config::Mqtt, client_id: &str) -> Result<MqttOptions, String> {
    let mut opts = MqttOptions::new(client_id, m.host.trim(), m.port);
    opts.set_keep_alive(Duration::from_secs(30));
    opts.set_max_packet_size(64 * 1024, 1024 * 1024); // (incoming: commands, outgoing: JPEG frames)
    if !m.username.is_empty() {
        opts.set_credentials(&m.username, &m.password);
    }
    if m.tls {
        // OpenSSL with the system CA bundle (ca-certificates)
        let c = rumqttc::tokio_native_tls::native_tls::TlsConnector::new().map_err(|e| format!("TLS: {e}"))?;
        opts.set_transport(Transport::tls_with_config(rumqttc::TlsConfiguration::NativeConnector(c)));
    }
    Ok(opts)
}

/// Try a broker without changing anything (the page's Test button): the
/// broker's answer to a connection, within 6 s.
pub async fn test(m: &config::Mqtt) -> Result<(), String> {
    if m.host.trim().is_empty() {
        return Err("no broker address".into());
    }
    let opts = options(m, &format!("{}-test", device_id()))?;
    let (client, mut ev) = AsyncClient::new(opts, 4);
    let r = tokio::time::timeout(Duration::from_secs(6), async {
        loop {
            match ev.poll().await {
                Ok(Event::Incoming(Packet::ConnAck(_))) => return Ok(()),
                Ok(_) => {}
                Err(e) => return Err(e.to_string()),
            }
        }
    })
    .await
    .unwrap_or_else(|_| Err("no answer in 6 s".into()));
    let _ = client.disconnect().await;
    r
}

/// Run until the task is dropped; reconnects on its own (rumqttc).
pub async fn run(m: config::Mqtt, ctrl: Control, bus: Bus, live: Arc<Live>, version: String) {
    let id = device_id();
    let base = format!("{}/{id}", m.base_topic);
    let mut opts = match options(&m, &id) {
        Ok(o) => o,
        Err(e) => {
            crate::error!("mqtt: {e}");
            bus.emit("mqtt", &serde_json::json!({ "connected": false, "error": e }));
            return;
        }
    };
    opts.set_last_will(LastWill::new(format!("{base}/status"), "offline", QoS::AtLeastOnce, true));
    let (client, eventloop) = AsyncClient::new(opts, 64);
    crate::info!("mqtt: {}:{} as {id}{}", m.host, m.port, if m.tls { " (TLS)" } else { "" });

    // the connection, its state, the commands
    let conn = tokio::spawn(connection(eventloop, client.clone(), m.clone(), base.clone(), id.clone(), ctrl.clone(),
                                       bus.clone(), version));
    let _conn = crate::web::AbortOnDrop(conn);

    // the camera entity: a frame every camera_interval_s
    let _cam = (m.camera_interval_s > 0).then(|| {
        let (client, live, topic) = (client.clone(), live.clone(), format!("{base}/camera"));
        let every = Duration::from_secs(m.camera_interval_s as u64);
        crate::web::AbortOnDrop(tokio::spawn(async move {
            let mut t = tokio::time::interval(every);
            loop {
                t.tick().await;
                if let Some(jpg) = live.snapshot().await {
                    let _ = client.publish(&topic, QoS::AtMostOnce, false, jpg.to_vec()).await;
                }
            }
        }))
    });

    // the events out
    let mut rx = bus.subscribe();
    let mut last_media: Option<serde_json::Value> = None;
    loop {
        let line = match rx.recv().await {
            Ok(l) => l,
            Err(broadcast::error::RecvError::Lagged(_)) => continue,
            Err(broadcast::error::RecvError::Closed) => return,
        };
        let Ok(ev) = serde_json::from_str::<serde_json::Value>(&line) else { continue };
        match ev.get("type").and_then(|t| t.as_str()) {
            Some("target_arrived") => {
                let _ = client.publish(format!("{base}/target"), QoS::AtLeastOnce, true, "ON").await;
                let _ = client.publish(format!("{base}/event"), QoS::AtLeastOnce, false, line.trim_end()).await;
                if let Some(jpg) = live.snapshot().await {
                    let _ = client.publish(format!("{base}/snapshot"), QoS::AtMostOnce, false, jpg.to_vec()).await;
                }
            }
            Some("target_left") => {
                let _ = client.publish(format!("{base}/target"), QoS::AtLeastOnce, true, "OFF").await;
                let _ = client.publish(format!("{base}/event"), QoS::AtLeastOnce, false, line.trim_end()).await;
            }
            Some("media") => last_media = Some(ev),
            Some("llm") => {
                let _ = client.publish(format!("{base}/llm"), QoS::AtLeastOnce, true, line.trim_end()).await;
            }
            Some("telemetry") => {
                let mut t = ev.clone();
                if let (Some(o), Some(m)) = (t.as_object_mut(), &last_media) {
                    o.insert("yolo_fps".into(), m.get("yolo_fps").cloned().unwrap_or_default());
                    o.insert("stream_fps".into(), m.get("ch1_fps").cloned().unwrap_or_default());
                }
                let _ = client.publish(format!("{base}/telemetry"), QoS::AtMostOnce, true, t.to_string()).await;
            }
            Some("config_reloaded") => {
                let c = ctrl.config.borrow().clone();
                publish_states(&client, &base, &c).await
            }
            _ => {}
        }
    }
}

async fn publish_states(client: &AsyncClient, base: &str, c: &Config) {
    let on = |b: bool| if b { "ON" } else { "OFF" };
    let _ = client.publish(format!("{base}/detection"), QoS::AtLeastOnce, true, on(c.detection.enabled)).await;
    let _ = client.publish(format!("{base}/led"), QoS::AtLeastOnce, true, on(c.led.brightness > 0)).await;
    let _ = client
        .publish(format!("{base}/led/brightness"), QoS::AtLeastOnce, true, c.led.brightness.to_string())
        .await;
}

#[allow(clippy::too_many_arguments)]
async fn connection(mut ev: EventLoop, client: AsyncClient, m: config::Mqtt, base: String, id: String, ctrl: Control,
                    bus: Bus, version: String) {
    let mut connected = false;
    loop {
        match ev.poll().await {
            Ok(Event::Incoming(Packet::ConnAck(_))) => {
                connected = true;
                crate::info!("mqtt: connected");
                bus.emit("mqtt", &serde_json::json!({ "connected": true }));
                let _ = client.publish(format!("{base}/status"), QoS::AtLeastOnce, true, "online").await;
                for t in ["detection/set", "led/set", "led/brightness/set"] {
                    let _ = client.subscribe(format!("{base}/{t}"), QoS::AtLeastOnce).await;
                }
                if m.discovery {
                    discovery(&client, &m, &base, &id, &version).await;
                }
                let c = ctrl.config.borrow().clone();
                publish_states(&client, &base, &c).await;
            }
            Ok(Event::Incoming(Packet::Publish(p))) => {
                let payload = String::from_utf8_lossy(&p.payload).trim().to_string();
                let topic = p.topic.strip_prefix(&format!("{base}/")).unwrap_or("").to_string();
                command(&ctrl, &topic, &payload).await;
            }
            Ok(_) => {}
            Err(e) => {
                if connected {
                    crate::warn!("mqtt: {e}");
                }
                connected = false;
                bus.emit("mqtt", &serde_json::json!({ "connected": false, "error": e.to_string() }));
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    }
}

/// A command from Home Assistant: change the configuration as the page does.
async fn command(ctrl: &Control, topic: &str, payload: &str) {
    let mut c = ctrl.config.borrow().clone();
    match topic {
        "detection/set" => c.detection.enabled = payload.eq_ignore_ascii_case("ON"),
        "led/set" => {
            if payload.eq_ignore_ascii_case("OFF") {
                c.led.brightness = 0;
            } else if c.led.brightness == 0 {
                c.led.brightness = 255;
            }
        }
        "led/brightness/set" => match payload.parse::<u32>() {
            Ok(b) => c.led.brightness = b.min(255),
            Err(_) => return,
        },
        _ => return,
    }
    if c == *ctrl.config.borrow() {
        return;
    }
    crate::info!("mqtt: {topic} {payload}");
    if let Err(e) = config::save(&ctrl.config_path, &c) {
        crate::error!("mqtt: save: {e}");
        return;
    }
    let _ = ctrl.reload.send(()).await;
}

/// Home Assistant MQTT discovery: one device with its entities.
async fn discovery(client: &AsyncClient, m: &config::Mqtt, base: &str, id: &str, version: &str) {
    let mut device = serde_json::json!({
        "identifiers": [id],
        "name": m.device_name,
        "manufacturer": "Seeed Studio (hardware), recamera-linux-kernel7 (software)",
        "model": "reCamera (SG2002, Linux 7.2)",
        "sw_version": version,
    });
    // "Visit device" in Home Assistant opens the camera's page
    if let Some(ip) = crate::web::addresses().into_iter().find(|(n, _)| n == "wlan0" || n == "eth0").map(|(_, a)| a)
        && let Some(o) = device.as_object_mut()
    {
        o.insert("configuration_url".into(), format!("http://{ip}/").into());
    }
    let avail = format!("{base}/status");
    let tele = format!("{base}/telemetry");
    let entities = [
        ("binary_sensor", "target", serde_json::json!({
            "name": "Target", "state_topic": format!("{base}/target"), "device_class": "occupancy",
        })),
        ("camera", "camera", serde_json::json!({
            "name": "Camera", "topic": format!("{base}/camera"),
        })),
        ("image", "snapshot", serde_json::json!({
            "name": "Last arrival", "image_topic": format!("{base}/snapshot"), "content_type": "image/jpeg",
        })),
        ("sensor", "llm", serde_json::json!({
            "name": "LLM answer", "state_topic": format!("{base}/llm"),
            // a state holds 255 characters at most; the whole answer is an attribute
            "value_template": "{{ (value_json.text if value_json.ok else 'error: ' ~ value_json.error)[:250] }}",
            "json_attributes_topic": format!("{base}/llm"), "icon": "mdi:robot-outline",
        })),
        ("sensor", "temperature", serde_json::json!({
            "name": "SoC temperature", "state_topic": tele, "value_template": "{{ value_json.soc_temp_c }}",
            "unit_of_measurement": "°C", "device_class": "temperature", "entity_category": "diagnostic",
        })),
        ("sensor", "load", serde_json::json!({
            "name": "Load", "state_topic": tele, "value_template": "{{ value_json.load1 }}",
            "entity_category": "diagnostic",
        })),
        ("sensor", "memory", serde_json::json!({
            "name": "Free memory", "state_topic": tele, "value_template": "{{ (value_json.mem_available_kb / 1024) | round(0) }}",
            "unit_of_measurement": "MB", "entity_category": "diagnostic",
        })),
        ("sensor", "yolo_fps", serde_json::json!({
            "name": "YOLO rate", "state_topic": tele, "value_template": "{{ value_json.yolo_fps }}",
            "unit_of_measurement": "fps", "entity_category": "diagnostic",
        })),
        ("switch", "detection", serde_json::json!({
            "name": "Detection", "state_topic": format!("{base}/detection"),
            "command_topic": format!("{base}/detection/set"), "entity_category": "config",
        })),
        ("light", "led", serde_json::json!({
            "name": "White LED", "state_topic": format!("{base}/led"), "command_topic": format!("{base}/led/set"),
            "brightness_state_topic": format!("{base}/led/brightness"),
            "brightness_command_topic": format!("{base}/led/brightness/set"), "brightness_scale": 255,
        })),
    ];
    for (component, object, mut cfg) in entities {
        if let Some(o) = cfg.as_object_mut() {
            o.insert("unique_id".into(), format!("{id}_{object}").into());
            o.insert("object_id".into(), format!("{id}_{object}").into());
            o.insert("availability_topic".into(), avail.clone().into());
            o.insert("device".into(), device.clone());
        }
        let topic = format!("{}/{component}/{id}/{object}/config", m.discovery_prefix);
        let _ = client.publish(topic, QoS::AtLeastOnce, true, cfg.to_string()).await;
    }
}

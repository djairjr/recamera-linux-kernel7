//! Periodic telemetry event: SoC temperature, load, free memory and the
//! daemon's own memory. Read from /proc and /sys only (no extra crates); the
//! thermal zone is the cv181x tempsen of patch 0021.

use serde::Serialize;
use std::fs;
use std::time::Duration;

#[derive(Serialize, Default)]
pub struct Sample {
    pub soc_temp_c: Option<f64>,
    pub load1: Option<f64>,
    pub mem_available_kb: Option<u64>,
    pub rss_kb: Option<u64>,
    pub uptime_s: Option<u64>,
}

/// one decimal: 43.4, not 43.395999908447266
fn round1(x: f64) -> f64 {
    (x * 10.0).round() / 10.0
}

fn read(path: &str) -> Option<String> {
    fs::read_to_string(path).ok()
}

pub fn sample() -> Sample {
    let page_kb = (unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as u64).max(1) / 1024;
    Sample {
        soc_temp_c: read("/sys/class/thermal/thermal_zone0/temp")
            .and_then(|s| s.trim().parse::<f64>().ok())
            .map(|m| round1(m / 1000.0)),
        load1: read("/proc/loadavg").and_then(|s| s.split_whitespace().next()?.parse().ok()),
        mem_available_kb: read("/proc/meminfo").and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("MemAvailable:"))?
                .split_whitespace()
                .nth(1)?
                .parse()
                .ok()
        }),
        // statm: size resident shared ... (pages)
        rss_kb: read("/proc/self/statm")
            .and_then(|s| s.split_whitespace().nth(1)?.parse::<u64>().ok())
            .map(|p| p * page_kb),
        uptime_s: read("/proc/uptime").and_then(|s| s.split('.').next()?.parse().ok()),
    }
}

/// Emit a telemetry event every `interval` until the task is dropped.
pub async fn run(bus: crate::events::Bus, interval: Duration) {
    let mut t = tokio::time::interval(interval);
    loop {
        t.tick().await;
        bus.emit("telemetry", &sample());
    }
}

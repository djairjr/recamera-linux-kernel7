//! System update from the web page (stage A8d): a recamera-update-*.rcu
//! (made by post-image.sh) streamed into the INACTIVE eMMC slot, the same
//! steps and checks as scripts/slot-update.sh. The running slot, the fip and
//! /data are never touched; a failed or interrupted upload only spoils the
//! slot that is not in use.
//!
//! The package is a tar: manifest.json (first), boot.itb (the kernel FIT),
//! rootfs.ext4.zst (the rootfs image, zstd). The upload is decoded while it
//! arrives (a blocking thread fed by a channel): nothing is kept whole in
//! memory or on disk but the 5 MB kernel.

use bytes::Bytes;
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::process::Command;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::mpsc;

const EMMC: &str = "/dev/mmcblk0";
const BOOT_MNT: &str = "/run/recamerad/boot";
/// a package is far smaller (~25 MB); anything bigger is not one
const MAX_PACKAGE: u64 = 512 << 20;
const MAX_KERNEL: u64 = 32 << 20;

/// What the page shows while and after an update.
#[derive(Serialize, Clone, Default)]
pub struct State {
    /// idle, receiving, verifying, finishing, ready, error
    pub phase: String,
    pub target: Option<String>,
    pub version: Option<String>,
    pub received: u64,
    pub written: u64,
    pub total: u64,
    pub message: String,
}

pub struct Updater {
    busy: AtomicBool,
    state: Mutex<State>,
}

impl Updater {
    pub fn new() -> Updater {
        Updater { busy: AtomicBool::new(false), state: Mutex::new(State { phase: "idle".into(), ..State::default() }) }
    }

    pub fn state(&self) -> State {
        self.state.lock().map(|s| s.clone()).unwrap_or_default()
    }

    fn set(&self, f: impl FnOnce(&mut State)) {
        if let Ok(mut s) = self.state.lock() {
            f(&mut s);
        }
    }

    /// Take the update slot (one update at a time).
    pub fn begin(&self) -> Result<Busy<'_>, String> {
        if self.busy.swap(true, Ordering::AcqRel) {
            return Err("an update is already running".into());
        }
        Ok(Busy(self))
    }
}

pub struct Busy<'a>(&'a Updater);

impl Drop for Busy<'_> {
    fn drop(&mut self) {
        self.0.busy.store(false, Ordering::Release);
    }
}

/// The slots: the one running (from root=) and the other.
pub fn slots() -> Result<(&'static str, &'static str), String> {
    let cmdline = std::fs::read_to_string("/proc/cmdline").map_err(|e| e.to_string())?;
    match cmdline.split_whitespace().find_map(|a| a.strip_prefix("root=")) {
        Some("/dev/mmcblk0p2") => Ok(("a", "b")),
        Some("/dev/mmcblk0p3") => Ok(("b", "a")),
        r => Err(format!("not running from an eMMC slot (root={}): update from the SD card with emmc-install",
                         r.unwrap_or("?"))),
    }
}

fn part(slot: &str) -> String {
    format!("{EMMC}p{}", if slot == "a" { 2 } else { 3 })
}

/// The version written in a slot's rootfs (mounted read-only for a moment).
pub fn slot_version(slot: &str) -> Option<String> {
    if slots().ok()?.0 == slot {
        return std::fs::read_to_string("/etc/recamera-version").ok().map(|s| s.trim().to_string());
    }
    let dir = "/run/recamerad/peek";
    std::fs::create_dir_all(dir).ok()?;
    let ok = Command::new("/bin/mount").args(["-o", "ro", &part(slot), dir]).status().ok()?.success();
    if !ok {
        return None;
    }
    let v = std::fs::read_to_string(format!("{dir}/etc/recamera-version")).ok().map(|s| s.trim().to_string());
    let _ = Command::new("/bin/umount").arg(dir).status();
    v
}

/// The upload as a `Read`: the HTTP body chunks come through a channel.
struct ChanReader {
    rx: mpsc::Receiver<Bytes>,
    cur: Bytes,
    got: u64,
    up: &'static Updater,
}

impl Read for ChanReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        while self.cur.is_empty() {
            match self.rx.blocking_recv() {
                Some(b) => {
                    self.got += b.len() as u64;
                    if self.got > MAX_PACKAGE {
                        return Err(io::Error::other("larger than any update package"));
                    }
                    let got = self.got;
                    self.up.set(|s| s.received = got);
                    self.cur = b;
                }
                None => return Ok(0),
            }
        }
        let n = buf.len().min(self.cur.len());
        buf[..n].copy_from_slice(&self.cur[..n]);
        self.cur = self.cur.slice(n..);
        Ok(n)
    }
}

#[derive(serde::Deserialize)]
struct Item {
    file: String,
    size: u64,
    sha256: String,
}

#[derive(serde::Deserialize)]
struct Manifest {
    format: u32,
    version: String,
    kernel: Item,
    rootfs: Item,
}

/// Write the package coming through `rx` into the inactive slot (blocking:
/// run it on its own thread). On success the slot is ready to boot.
pub fn install(up: &'static Updater, rx: mpsc::Receiver<Bytes>) -> Result<String, String> {
    let r = install_inner(up, rx);
    match &r {
        Ok(v) => {
            crate::info!("update: {v} written to slot {}", up.state().target.unwrap_or_default());
            up.set(|s| {
                s.phase = "ready".into();
                s.message = format!("{v} is in slot {}: reboot into it", s.target.clone().unwrap_or_default());
            });
        }
        Err(e) => {
            crate::error!("update: {e}");
            up.set(|s| {
                s.phase = "error".into();
                s.message = e.clone();
            });
        }
    }
    r
}

fn run(cmd: &str, args: &[&str]) -> Result<(), String> {
    let st = Command::new(cmd).args(args).status().map_err(|e| format!("{cmd}: {e}"))?;
    if st.success() { Ok(()) } else { Err(format!("{cmd} {}: exit {}", args.join(" "), st.code().unwrap_or(-1))) }
}

fn hex(d: &[u8]) -> String {
    d.iter().map(|b| format!("{b:02x}")).collect()
}

fn install_inner(up: &'static Updater, rx: mpsc::Receiver<Bytes>) -> Result<String, String> {
    let (_, target) = slots()?;
    let dev = part(target);
    if std::fs::read_to_string("/proc/mounts").unwrap_or_default().lines().any(|l| l.starts_with(&dev)) {
        return Err(format!("{dev} is mounted"));
    }
    up.set(|s| *s = State { phase: "receiving".into(), target: Some(target.into()), ..State::default() });

    let reader = ChanReader { rx, cur: Bytes::new(), got: 0, up };
    let mut ar = tar::Archive::new(reader);
    let mut entries = ar.entries().map_err(|e| format!("not an update package: {e}"))?;
    let mut next = |want: &str| -> Result<tar::Entry<'_, ChanReader>, String> {
        let e = entries.next().ok_or(format!("the upload ended before {want}"))?
            .map_err(|_| "not an update package (a recamera-update-*.rcu), or the upload was cut".to_string())?;
        let name = e.path().map_err(|e| e.to_string())?.to_string_lossy().into_owned();
        if name != want {
            return Err(format!("package has {name} where {want} should be"));
        }
        Ok(e)
    };

    // 1. the manifest
    let mut m = String::new();
    next("manifest.json")?.take(64 * 1024).read_to_string(&mut m).map_err(|e| e.to_string())?;
    let m: Manifest = serde_json::from_str(&m).map_err(|e| format!("manifest: {e}"))?;
    if m.format != 1 || m.kernel.file != "boot.itb" || m.rootfs.file != "rootfs.ext4.zst" {
        return Err(format!("manifest format {} not known to this version", m.format));
    }
    if m.kernel.size > MAX_KERNEL {
        return Err("kernel too large".into());
    }
    let psize = std::fs::read_to_string(format!("/sys/class/block/{}/size", dev.trim_start_matches("/dev/")))
        .ok().and_then(|s| s.trim().parse::<u64>().ok()).unwrap_or(0) * 512;
    if m.rootfs.size == 0 || m.rootfs.size > psize {
        return Err(format!("rootfs of {} bytes does not fit {dev} ({psize} bytes)", m.rootfs.size));
    }
    crate::info!("update: {} into slot {target} ({dev})", m.version);
    up.set(|s| {
        s.version = Some(m.version.clone());
        s.total = m.rootfs.size;
    });

    // 2. the kernel, kept in memory until the rootfs is in
    let mut kernel = Vec::with_capacity(m.kernel.size as usize);
    next("boot.itb")?.take(MAX_KERNEL).read_to_end(&mut kernel).map_err(|e| e.to_string())?;
    if kernel.len() as u64 != m.kernel.size || hex(&Sha256::digest(&kernel)) != m.kernel.sha256 {
        return Err("boot.itb does not match the manifest".into());
    }

    // 3. the slot's kernel goes first: a slot half written must never be
    // booted (emmcboot_other would start it if the running kernel went
    // missing); the new kernel is written only when everything checked out
    with_boot(|b| match std::fs::remove_file(format!("{b}/boot-{target}.itb")) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(format!("boot-{target}.itb: {e}")),
        _ => Ok(()),
    })?;

    // 4. the rootfs, decoded straight onto the partition
    let entry = next("rootfs.ext4.zst")?;
    let mut z = ruzstd::decoding::StreamingDecoder::new(entry).map_err(|e| format!("rootfs.ext4.zst: {e}"))?;
    let mut out = OpenOptions::new().write(true).open(&dev).map_err(|e| format!("{dev}: {e}"))?;
    let mut sha = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    let mut written = 0u64;
    loop {
        // ruzstd wraps the reader's end of data in its own error
        let n = z.read(&mut buf).map_err(|e| {
            if e.kind() == io::ErrorKind::UnexpectedEof || e.to_string().contains("UnexpectedEof") {
                "the upload ended before the package did".to_string()
            } else {
                format!("rootfs.ext4.zst: {e}")
            }
        })?;
        if n == 0 {
            break;
        }
        written += n as u64;
        if written > m.rootfs.size {
            return Err("rootfs larger than the manifest says".into());
        }
        sha.update(&buf[..n]);
        out.write_all(&buf[..n]).map_err(|e| format!("{dev}: {e}"))?;
        up.set(|s| s.written = written);
    }
    out.sync_all().map_err(|e| format!("{dev}: {e}"))?;
    drop(out);
    if written != m.rootfs.size || hex(&sha.finalize()) != m.rootfs.sha256 {
        return Err("rootfs does not match the manifest (corrupted package?)".into());
    }

    // 5. read it back from the eMMC, as slot-update.sh does
    up.set(|s| s.phase = "verifying".into());
    let _ = std::fs::write("/proc/sys/vm/drop_caches", "3");
    let mut f = File::open(&dev).map_err(|e| format!("{dev}: {e}"))?;
    f.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    let mut sha = Sha256::new();
    let mut left = m.rootfs.size;
    while left > 0 {
        let n = (left as usize).min(buf.len());
        f.read_exact(&mut buf[..n]).map_err(|e| format!("{dev}: {e}"))?;
        sha.update(&buf[..n]);
        left -= n as u64;
    }
    if hex(&sha.finalize()) != m.rootfs.sha256 {
        return Err(format!("{dev} reads back different from what was written"));
    }

    // 6. grow it to the partition, its own label and UUID, then the kernel
    up.set(|s| s.phase = "finishing".into());
    let st = Command::new("/sbin/e2fsck").args(["-fy", &dev]).status().map_err(|e| format!("e2fsck: {e}"))?;
    if st.code().unwrap_or(8) > 1 {
        return Err(format!("e2fsck {dev}: exit {}", st.code().unwrap_or(-1)));
    }
    run("/sbin/resize2fs", &[&dev])?;
    run("/sbin/tune2fs", &["-L", &format!("rootfs-{target}"), "-U", "random", &dev])?;

    with_boot(|b| {
        let new = format!("{b}/boot-{target}.itb.new");
        let mut f = File::create(&new).map_err(|e| format!("{new}: {e}"))?;
        f.write_all(&kernel).and_then(|_| f.sync_all()).map_err(|e| format!("{new}: {e}"))?;
        drop(f);
        let _ = std::fs::write("/proc/sys/vm/drop_caches", "3");
        let back = std::fs::read(&new).map_err(|e| format!("{new}: {e}"))?;
        if hex(&Sha256::digest(&back)) != m.kernel.sha256 {
            return Err(format!("{new} reads back different"));
        }
        std::fs::rename(&new, format!("{b}/boot-{target}.itb")).map_err(|e| e.to_string())
    })?;
    Ok(m.version)
}

/// Run `f` with the eMMC boot partition (the kernels) mounted.
fn with_boot<T>(f: impl FnOnce(&str) -> Result<T, String>) -> Result<T, String> {
    std::fs::create_dir_all(BOOT_MNT).map_err(|e| e.to_string())?;
    run("/bin/mount", &[&format!("{EMMC}p1"), BOOT_MNT])?;
    let r = f(BOOT_MNT);
    let _ = Command::new("/bin/sync").status();
    let _ = Command::new("/bin/umount").arg(BOOT_MNT).status();
    r
}

/// Boot the updated slot next.
pub fn switch_to(slot: &str) -> Result<(), String> {
    run("/usr/sbin/fw_setenv", &["slot", slot])?;
    let _ = Command::new("/bin/sync").status();
    Ok(())
}

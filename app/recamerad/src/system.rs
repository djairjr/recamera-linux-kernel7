//! System actions of the page (stage A8): models in /data/models, the
//! password of a system account, the time zone, reboot, logs and crash
//! reports.
//!
//! Things that must outlive an A/B update (the rootfs of each slot is
//! rewritten) live in /data/recamera and are put back at boot by the init
//! scripts: the password hashes (S02passwords) and the time zone
//! (/etc/localtime -> /data/recamera/localtime).

use std::ffi::{CStr, CString, c_char};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

pub const MODELS_DIR: &str = "/data/models";
const PASSWORDS: &str = "/data/recamera/passwords";
const LOCALTIME: &str = "/data/recamera/localtime";
const ZONEINFO: &str = "/usr/share/zoneinfo";
const CRASH_DIR: &str = "/data/recamera/crash";
/// a .cvimodel starts with this
const CVIMODEL_MAGIC: &[u8] = b"CviModel";
pub const MAX_MODEL: u64 = 32 * 1024 * 1024;

/// A file name we accept in /data/models (no path, no hidden files).
pub fn model_name_ok(n: &str) -> bool {
    !n.is_empty()
        && n.len() <= 128
        && !n.starts_with('.')
        && n.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
        && (n.ends_with(".cvimodel") || n.ends_with(".json"))
}

/// The models in /data/models: name, size, title and number of classes from
/// the .json next to it.
pub fn models(active: &str) -> Vec<serde_json::Value> {
    let mut out = Vec::new();
    let Ok(dir) = fs::read_dir(MODELS_DIR) else { return out };
    let mut names: Vec<String> =
        dir.filter_map(|e| e.ok()?.file_name().into_string().ok()).filter(|n| n.ends_with(".cvimodel")).collect();
    names.sort();
    for n in names {
        let p = Path::new(MODELS_DIR).join(&n);
        let size = fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
        let meta = fs::read(p.with_extension("json")).ok()
            .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok());
        let title = meta.as_ref().and_then(|m| m.get("model_name")).and_then(|v| v.as_str()).map(String::from);
        let classes = meta.as_ref().and_then(|m| m.get("classes")).and_then(|v| v.as_array()).map(|a| a.len());
        // for the list only: the .json's "task", else a guess from the names
        // (the daemon finds the real one from the outputs when it loads it)
        let task = meta.as_ref().and_then(|m| m.get("task")).and_then(|v| v.as_str()).map(String::from)
            .unwrap_or_else(|| {
                let s = format!("{n} {}", title.as_deref().unwrap_or("")).to_lowercase();
                if s.contains("classif") { "classify" } else if s.contains("pose") { "pose" }
                else if s.contains("seg") { "segment" } else { "detect" }.to_string()
            });
        out.push(serde_json::json!({
            "name": n, "size": size, "title": title, "classes": classes, "task": task, "active": n == active,
        }));
    }
    out
}

/// The temporary file an upload is written to.
pub fn model_tmp(name: &str) -> PathBuf {
    Path::new(MODELS_DIR).join(format!(".{name}.part"))
}

/// Check an uploaded model file (a .cvimodel starts with its magic, a .json
/// parses) and put it in place of the old one.
pub fn finish_model(name: &str) -> Result<(), String> {
    let tmp = model_tmp(name);
    let check = || -> Result<(), String> {
        if name.ends_with(".cvimodel") {
            use std::io::Read;
            let mut head = [0u8; 8];
            fs::File::open(&tmp).and_then(|mut f| f.read_exact(&mut head)).map_err(|e| e.to_string())?;
            if head != CVIMODEL_MAGIC {
                return Err(format!("{name}: not a .cvimodel (no CviModel header)"));
            }
        } else {
            let b = fs::read(&tmp).map_err(|e| e.to_string())?;
            serde_json::from_slice::<serde_json::Value>(&b).map_err(|e| format!("{name}: {e}"))?;
        }
        fs::File::open(&tmp).and_then(|f| f.sync_all()).map_err(|e| e.to_string())?;
        fs::rename(&tmp, Path::new(MODELS_DIR).join(name)).map_err(|e| e.to_string())
    };
    let r = check();
    if r.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    r
}

pub fn delete_model(name: &str, active: &str) -> Result<(), String> {
    if !model_name_ok(name) {
        return Err(format!("{name:?}: not a model file name"));
    }
    if name == active {
        return Err(format!("{name} is the active model"));
    }
    let p = Path::new(MODELS_DIR).join(name);
    fs::remove_file(&p).map_err(|e| format!("{name}: {e}"))?;
    if name.ends_with(".cvimodel") {
        let _ = fs::remove_file(p.with_extension("json"));
    }
    Ok(())
}

unsafe extern "C" {
    fn crypt(key: *const c_char, salt: *const c_char) -> *mut c_char;
}

/// SHA-256-crypt ($5$) hash with a new random salt.
fn hash_password(password: &str) -> Option<String> {
    use std::io::Read;
    const SALT: &[u8] = b"./0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
    let mut r = [0u8; 16];
    fs::File::open("/dev/urandom").and_then(|mut f| f.read_exact(&mut r)).ok()?;
    let salt: String = r.iter().map(|b| SALT[(*b as usize) % SALT.len()] as char).collect();
    let (k, s) = (CString::new(password).ok()?, CString::new(format!("$5${salt}$")).ok()?);
    // SAFETY: valid C strings; the static result is copied at once (one
    // network thread)
    let h = unsafe { crypt(k.as_ptr(), s.as_ptr()) };
    if h.is_null() {
        return None;
    }
    let h = unsafe { CStr::from_ptr(h) }.to_string_lossy().into_owned();
    h.starts_with("$5$").then_some(h)
}

/// Replace the hash of `user` in a shadow-like file ("user:hash:..."), or
/// add "user:hash" when `add` and the user is not there; atomic.
fn set_hash_in(path: &str, user: &str, hash: &str, add: bool) -> Result<(), String> {
    let text = fs::read_to_string(path).unwrap_or_default();
    let mut found = false;
    let mut out = String::with_capacity(text.len() + 64);
    for l in text.lines() {
        let mut f: Vec<&str> = l.split(':').collect();
        if f.first() == Some(&user) && f.len() >= 2 {
            f[1] = hash;
            found = true;
            out.push_str(&f.join(":"));
        } else {
            out.push_str(l);
        }
        out.push('\n');
    }
    if !found {
        if !add {
            return Err(format!("no user {user}"));
        }
        out.push_str(&format!("{user}:{hash}\n"));
    }
    let tmp = format!("{path}.new");
    let mut f = fs::File::create(&tmp).map_err(|e| e.to_string())?;
    f.write_all(out.as_bytes()).and_then(|_| f.sync_all()).map_err(|e| e.to_string())?;
    #[allow(clippy::permissions_set_readonly_false)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&tmp, fs::Permissions::from_mode(0o600));
    }
    fs::rename(&tmp, path).map_err(|e| e.to_string())
}

/// New password of a system account: /etc/shadow now, /data/recamera for the
/// next slots (S02passwords puts it back at boot).
pub fn set_password(user: &str, password: &str) -> Result<(), String> {
    if password.len() < 6 {
        return Err("at least 6 characters".into());
    }
    let hash = hash_password(password).ok_or("cannot hash the password")?;
    set_hash_in("/etc/shadow", user, &hash, false)?;
    if let Some(d) = Path::new(PASSWORDS).parent() {
        let _ = fs::create_dir_all(d);
    }
    set_hash_in(PASSWORDS, user, &hash, true)
}

/// The zone names available (for the page).
pub fn timezones() -> Vec<String> {
    fn walk(dir: &Path, prefix: &str, out: &mut Vec<String>) {
        let Ok(rd) = fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let n = e.file_name().to_string_lossy().into_owned();
            if n.contains('.') || n == "posix" || n == "right" || n.chars().next().is_some_and(|c| c.is_lowercase()) {
                continue;
            }
            let name = if prefix.is_empty() { n.clone() } else { format!("{prefix}/{n}") };
            // the top level names are symlinks into posix/: follow them
            match fs::metadata(e.path()) {
                Ok(m) if m.is_dir() => walk(&e.path(), &name, out),
                Ok(_) => out.push(name),
                Err(_) => {}
            }
        }
    }
    let mut out = Vec::new();
    walk(Path::new(ZONEINFO), "", &mut out);
    out.sort();
    out
}

/// Point /data/recamera/localtime (where /etc/localtime points) at a zone.
pub fn apply_timezone(tz: &str) -> Result<(), String> {
    let target = PathBuf::from(ZONEINFO).join(tz);
    if tz != "UTC" && !target.is_file() {
        return Err(format!("time zone {tz}: not in {ZONEINFO}"));
    }
    let tmp = format!("{LOCALTIME}.new");
    let _ = fs::remove_file(&tmp);
    std::os::unix::fs::symlink(&target, &tmp).map_err(|e| e.to_string())?;
    fs::rename(&tmp, LOCALTIME).map_err(|e| e.to_string())
}

/// The last `n` lines of the system log.
pub fn log_tail(n: usize) -> String {
    let t = fs::read_to_string("/var/log/messages").unwrap_or_default();
    let lines: Vec<&str> = t.lines().collect();
    lines[lines.len().saturating_sub(n)..].join("\n")
}

/// Crash reports (pstore copies made at boot): the newest first.
pub fn crashes() -> Vec<serde_json::Value> {
    let Ok(rd) = fs::read_dir(CRASH_DIR) else { return Vec::new() };
    let mut v: Vec<(std::time::SystemTime, String, Vec<String>)> = rd
        .flatten()
        .filter(|e| e.path().is_dir())
        .map(|e| {
            let files = fs::read_dir(e.path())
                .map(|r| r.flatten().map(|f| f.file_name().to_string_lossy().into_owned()).collect())
                .unwrap_or_default();
            let t = e.metadata().and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH);
            (t, e.file_name().to_string_lossy().into_owned(), files)
        })
        .collect();
    v.sort_by(|a, b| b.0.cmp(&a.0));
    v.into_iter().map(|(_, n, f)| serde_json::json!({ "name": n, "files": f })).collect()
}

/// One file of a crash report (names checked: no path escapes).
pub fn crash_file(dir: &str, file: &str) -> Option<String> {
    let ok = |s: &str| !s.is_empty() && !s.contains('/') && !s.starts_with('.');
    if !ok(dir) || !ok(file) {
        return None;
    }
    fs::read_to_string(Path::new(CRASH_DIR).join(dir).join(file)).ok()
}

//! Logging: syslog(3) when running as a service (the camera's syslogd keeps
//! it in /var/log/messages), stderr in the foreground. No logging crate: four
//! levels and one line per message are all the daemon needs.

use std::ffi::CString;
use std::sync::OnceLock;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Level {
    Error,
    Warn,
    Info,
    Debug,
}

struct Logger {
    syslog: bool,
    max: Level,
}

static LOGGER: OnceLock<Logger> = OnceLock::new();

/// Set up once at start: `foreground` logs to stderr instead of syslog.
pub fn init(foreground: bool, verbose: bool) {
    let max = if verbose { Level::Debug } else { Level::Info };
    if !foreground {
        // openlog keeps the pointer: the identity must live forever
        static IDENT: &[u8] = b"recamerad\0";
        unsafe { libc::openlog(IDENT.as_ptr().cast(), libc::LOG_PID, libc::LOG_DAEMON) };
    }
    let _ = LOGGER.set(Logger { syslog: !foreground, max });
}

pub fn write(level: Level, msg: &str) {
    let Some(l) = LOGGER.get() else {
        eprintln!("{msg}");
        return;
    };
    if level > l.max {
        return;
    }
    if l.syslog {
        let prio = match level {
            Level::Error => libc::LOG_ERR,
            Level::Warn => libc::LOG_WARNING,
            Level::Info => libc::LOG_INFO,
            Level::Debug => libc::LOG_DEBUG,
        };
        if let Ok(c) = CString::new(msg.replace('\0', " ")) {
            unsafe { libc::syslog(prio, b"%s\0".as_ptr().cast(), c.as_ptr()) };
        }
    } else {
        eprintln!("recamerad: {:?}: {msg}", level);
    }
}

#[macro_export]
macro_rules! error { ($($a:tt)*) => { $crate::log::write($crate::log::Level::Error, &format!($($a)*)) } }
#[macro_export]
macro_rules! warn { ($($a:tt)*) => { $crate::log::write($crate::log::Level::Warn, &format!($($a)*)) } }
#[macro_export]
macro_rules! info { ($($a:tt)*) => { $crate::log::write($crate::log::Level::Info, &format!($($a)*)) } }
#[macro_export]
macro_rules! debug { ($($a:tt)*) => { $crate::log::write($crate::log::Level::Debug, &format!($($a)*)) } }

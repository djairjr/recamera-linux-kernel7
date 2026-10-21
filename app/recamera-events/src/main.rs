//! recamera-events -- print the recamerad event stream (one JSON object per
//! line) from its Unix socket, for pipes: `recamera-events | grep detection`.
//! BusyBox nc has no Unix socket support, hence this tool. std only.

use std::io::{self, Write};
use std::os::unix::net::UnixStream;
use std::process::ExitCode;

const DEFAULT: &str = "/run/recamerad/events.sock";

const HELP: &str = "\
Usage: recamera-events [SOCKET]

Print the events of recamerad (JSON lines) until it stops or the output is
closed. SOCKET: the events.socket of the recamerad configuration
(default /run/recamerad/events.sock).

  -h, --help      this help
  -V, --version   version
";

fn main() -> ExitCode {
    let mut path = DEFAULT.to_string();
    for a in std::env::args().skip(1) {
        match a.as_str() {
            "-h" | "--help" => {
                print!("{HELP}");
                return ExitCode::SUCCESS;
            }
            "-V" | "--version" => {
                println!("recamera-events {}", env!("CARGO_PKG_VERSION"));
                return ExitCode::SUCCESS;
            }
            _ if a.starts_with('-') => {
                eprintln!("recamera-events: unknown option {a}\nTry 'recamera-events --help'.");
                return ExitCode::from(2);
            }
            _ => path = a,
        }
    }
    let mut sock = match UnixStream::connect(&path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("recamera-events: {path}: {e} (is recamerad running?)");
            return ExitCode::FAILURE;
        }
    };
    let mut out = io::stdout().lock();
    match io::copy(&mut sock, &mut out).and_then(|_| out.flush()) {
        Ok(()) => ExitCode::SUCCESS,
        // the reader of a pipe went away: a normal end
        Err(e) if e.kind() == io::ErrorKind::BrokenPipe => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("recamera-events: {e}");
            ExitCode::FAILURE
        }
    }
}

//! opencode-spotlight: a spotlight-style OpenCode chat overlay for GNOME.

mod app;
mod bridge;
mod desktop;
mod oc;
mod state;
mod ui;

use std::io::Write;

fn main() {
    let mut args = std::env::args_os().skip(1);
    let code = match args.next().and_then(|a| a.into_string().ok()).as_deref() {
        Some("install") => match args.next().and_then(|a| a.into_string().ok()).as_deref() {
            None => desktop::cli_install(),
            Some(extra) => {
                eprintln!("opencode-spotlight: 'install' takes no arguments (got '{extra}')");
                2
            }
        },
        Some("uninstall") => match args.next().and_then(|a| a.into_string().ok()).as_deref() {
            None => desktop::cli_uninstall(),
            Some(extra) => {
                eprintln!(
                    "opencode-spotlight: 'uninstall' takes no arguments (got '{extra}'); it removes exactly what 'install' recorded"
                );
                2
            }
        },
        Some("help") => print_help(),
        Some("--help") | Some("-h") => print_help(),
        Some("--version") | Some("-V") => {
            env_report();
            0
        }
        Some("debug") => debug_client(args.by_ref().map(|a| a.to_string_lossy().into_owned()).collect::<Vec<_>>()),
        // Everything else (--toggle, the empty launch) goes through the GTK
        // application so D-Bus routing reaches the resident instance.
        _ => app::run(),
    };
    let _ = std::io::stdout().flush();
    std::process::exit(code);
}

fn print_help() -> i32 {
    println!(
        "usage: opencode-spotlight [--toggle]

  (no args)      start the resident app (no window until summoned)
  --toggle       show the overlay when hidden, hide it when shown
                 (this is what the GNOME keybinding should run)
  install        write the desktop entry, autostart entry, and keybinding
  uninstall      remove everything the install helper created
  --version      print version and debug info"
    );
    0
}

fn env_report() -> i32 {
    println!("opencode-spotlight {}", env!("CARGO_PKG_VERSION"));
    let paths = state::Paths::detect();
    println!("config: {}", paths.config_file().display());
    println!("state:  {}", paths.state_file().display());
    println!("scratch: {}", paths.scratch_dir().display());
    0
}

/// `opencode-spotlight debug <verb> [payload…]` — talk to a running app's
/// debug socket (requires the app launched with SPOTLIGHT_DEBUG_IPC=1).
fn debug_client(rest: Vec<String>) -> i32 {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;
    let mut it = rest.into_iter();
    let verb = it.next().unwrap_or_else(|| "state".into());
    let payload = it.collect::<Vec<_>>().join(" ");
    let runtime_dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".into());
    let path = format!("{runtime_dir}/{}", ui::debug_ipc::SOCKET_NAME);
    let Ok(mut stream) = UnixStream::connect(&path) else {
        eprintln!("opencode-spotlight: debug socket not active ({path}).");
        eprintln!("Start the app with SPOTLIGHT_DEBUG_IPC=1 to enable it.");
        return 1;
    };
    let command = if payload.is_empty() {
        verb
    } else {
        format!("{verb} {payload}")
    };
    stream.write_all(command.as_bytes()).ok();
    stream.write_all(b"\n").ok();
    stream.flush().ok();
    let mut reply = String::new();
    if let Ok(_reader) = BufReader::new(&stream).read_line(&mut reply) {
        println!("{reply}");
    }
    reply.contains("\"ok\":true") as i32
}

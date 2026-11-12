//! opencode-spotlight: a spotlight-style OpenCode chat overlay for GNOME.

mod app;
mod bridge;
mod desktop;
mod oc;
mod state;
mod ui;

use std::io::Write;

fn main() {
    let mut args = std::env::args().skip(1);
    let code = match args.next().as_deref() {
        Some("install") => desktop::cli_install(),
        Some("uninstall") => desktop::cli_uninstall(),
        Some("help") => print_help(),
        Some("--help") | Some("-h") => print_help(),
        Some("--version") | Some("-V") => {
            env_report();
            0
        }
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

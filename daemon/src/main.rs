//! Device daemon (prototype phase): serves the web UI and the device API under /api/v1, drives
//! the media programs as child processes and maps their statistics to the API's neutral fields.
//!
//! The runtime is single-threaded on purpose: every child is forked from this one long-lived
//! thread, so PR_SET_PDEATHSIG (bound to the forking thread) only fires when the daemon exits.

#[macro_use]
mod util;
mod api;
mod app;
mod check;
mod config;
mod events;
mod history;
mod media;
mod model;
mod preview;
mod profile;
mod stats;
mod store;
mod supervisor;

use std::path::PathBuf;

fn usage() -> ! {
    eprintln!("usage: ctrld [--config <device.toml>]   (default {})", config::default_config_path().display());
    std::process::exit(2)
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let mut path = config::default_config_path();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "-c" | "--config" => path = args.next().map(PathBuf::from).unwrap_or_else(|| usage()),
            "-V" | "--version" => {
                println!("ctrld {}", env!("CARGO_PKG_VERSION"));
                return;
            }
            _ => usage(),
        }
    }
    let cfg = match config::Config::load(&path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{}: {e}", path.display());
            std::process::exit(2);
        }
    };
    if let Err(e) = app::run(cfg).await {
        eprintln!("{e}");
        std::process::exit(1);
    }
}

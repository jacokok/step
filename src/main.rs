use clap::{Parser, Subcommand};
use playlist_mix::{
    models,
    pipeline::{self, MixArgs},
    process::Tools,
};
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    version,
    about = "Download a playlist and make a beat-aligned mix without tempo changes"
)]
struct Cli {
    #[command(subcommand)]
    command: Action,
}
#[derive(Subcommand)]
enum Action {
    /// Fetch and SHA-256-verify prebuilt models; no Python needed.
    Models {
        #[arg(long, default_value = "models")]
        dir: PathBuf,
        #[arg(long)]
        small: bool,
    },
    /// Download, analyze, plan, and optionally render a playlist mix.
    Mix(MixArgs),
}

fn main() {
    // SAFETY: this is the first operation in main, before any threads, parsing,
    // HTTP clients or model initialization. Override user thread settings so the
    // detector always uses the same single-thread configuration.
    unsafe {
        std::env::set_var("RTEN_NUM_THREADS", "1");
        std::env::set_var("RAYON_NUM_THREADS", "1");
    }
    let result = rayon::ThreadPoolBuilder::new()
        .num_threads(1)
        .build_global()
        .map_err(anyhow::Error::from)
        .and_then(|()| match Cli::parse().command {
            Action::Models { dir, small } => models::install(&dir, small),
            Action::Mix(args) => pipeline::run(args, &Tools::default()),
        });
    if let Err(error) = result {
        eprintln!("error: {error:#}");
        std::process::exit(1);
    }
}

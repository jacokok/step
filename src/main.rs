use clap::{Parser, Subcommand};
use std::path::PathBuf;
use step::{
    cut::{self, CutArgs},
    models,
    pipeline::{self, AllArgs, BeatArgs, GetArgs, MixArgs, Stage},
    process::Tools,
};

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
        #[arg(long, env = "STEP_MODEL_DIR", default_value_os_t = models::default_dir())]
        dir: PathBuf,
        #[arg(long)]
        small: bool,
    },
    /// Append audio from any yt-dlp-supported URL without analyzing or mixing.
    Get(GetArgs),
    /// Trim leading and trailing silence, keeping backups of originals.
    Cut(CutArgs),
    /// Analyze beats in downloaded or locally supplied tracks.
    Beat(BeatArgs),
    /// Mix previously analyzed tracks without downloading or detecting beats.
    Mix(MixArgs),
    /// Download, analyze, plan, and optionally render a playlist mix.
    All(AllArgs),
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
            Action::Get(args) => pipeline::get(args, &Tools::default()),
            Action::Cut(args) => cut::run(args, &Tools::default()),
            Action::Beat(args) => pipeline::beat(args, &Tools::default()),
            Action::Mix(args) => pipeline::run_stage(args, &Tools::default(), Stage::Mix),
            Action::All(mut args) => {
                args.mix.playlist_url = args.playlist_url;
                args.mix.model_dir = args.model_dir;
                args.mix.small_model = args.small_model;
                args.mix.cookies = args.cookies;
                pipeline::run(args.mix, &Tools::default())
            }
        });
    if let Err(error) = result {
        eprintln!("error: {error:#}");
        std::process::exit(1);
    }
}

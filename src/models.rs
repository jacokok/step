use crate::manifest::hash_file;
use anyhow::{Context, Result, ensure};
use std::{
    fs::{self, File},
    io,
    path::{Path, PathBuf},
};

const COMMIT: &str = "1ae768e78f1ad83b0ed3886241dc29ffde853c40";
pub const MEL_HASH: &str = "fdd59e65c515331308e4c8841edf99972deca646bdf6197744c2a5b7755e3de9";
pub const SMALL_HASH: &str = "a5f8d39d989f31859454ba27afe61c5317ca95e4d9373e6853e5361b8937172f";
pub const FULL_HASH: &str = "5f810debe53459b559127fb55bbad40035bb47cc567b20e501670f968c770f02";

/// Shared across working directories for an installed CLI. Explicit --dir/
/// --model-dir (or STEP_MODEL_DIR) always takes precedence over this default.
pub fn default_dir() -> PathBuf {
    directories::ProjectDirs::from("", "", "step")
        .map(|dirs| dirs.cache_dir().join("models"))
        .unwrap_or_else(|| PathBuf::from("models"))
}

pub fn paths(dir: &Path, small: bool) -> (PathBuf, PathBuf) {
    (
        dir.join("mel_spectrogram.onnx"),
        dir.join(if small {
            "beat_this_small.onnx"
        } else {
            "beat_this.onnx"
        }),
    )
}

pub fn verify(dir: &Path, small: bool) -> Result<String> {
    let (mel, beat) = paths(dir, small);
    for (path, expected) in [
        (&mel, MEL_HASH),
        (&beat, if small { SMALL_HASH } else { FULL_HASH }),
    ] {
        let actual = hash_file(path).with_context(|| {
            format!(
                "Model missing: {}. Run `step models --dir '{}' {}` first",
                path.display(),
                dir.display(),
                if small { "--small" } else { "" }
            )
        })?;
        ensure!(
            actual == expected,
            "Model checksum mismatch: {}. Remove it and rerun `step models`; refusing unpinned weights",
            path.display()
        );
    }
    Ok(format!(
        "beat-this=1.1.0;rten=0.24;mel={MEL_HASH};beat={};analysis=2;mono=22050;threads=1;arch={};os={}",
        if small { SMALL_HASH } else { FULL_HASH },
        std::env::consts::ARCH,
        std::env::consts::OS
    ))
}

pub fn install(dir: &Path, small: bool) -> Result<()> {
    fs::create_dir_all(dir)?;
    let (mel, beat) = paths(dir, small);
    let base = format!("https://raw.githubusercontent.com/danigb/beat-this-rs/{COMMIT}/models");
    let beat_url = if small {
        format!("{base}/beat_this_small.onnx")
    } else {
        "https://github.com/danigb/beat-this-rs/releases/download/model-large/beat_this.onnx".into()
    };
    for (path, url, expected) in [
        (&mel, format!("{base}/mel_spectrogram.onnx"), MEL_HASH),
        (&beat, beat_url, if small { SMALL_HASH } else { FULL_HASH }),
    ] {
        if path.is_file() && hash_file(path)? == expected {
            println!("Verified cached {}", path.display());
            continue;
        }
        println!("Downloading {}", path.display());
        let partial = path.with_extension("onnx.partial");
        let result = (|| -> Result<()> {
            let mut response = ureq::get(&url).call().with_context(|| {
                format!("Model download failed: {url}; check network access and retry")
            })?;
            let mut file = File::create(&partial)?;
            io::copy(&mut response.body_mut().as_reader(), &mut file)?;
            file.sync_all()?;
            ensure!(
                hash_file(&partial)? == expected,
                "Model SHA-256 mismatch; refusing downloaded weights"
            );
            fs::rename(&partial, path)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(partial);
        }
        result?;
    }
    verify(dir, small)?;
    Ok(())
}

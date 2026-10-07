use anyhow::{Context, Result, bail, ensure};
use std::{collections::BTreeMap, ffi::OsString, process::Command};

#[derive(Debug, Clone)]
pub struct Tools {
    pub yt_dlp: OsString,
    pub ffmpeg: OsString,
    pub ffprobe: OsString,
}
impl Default for Tools {
    fn default() -> Self {
        Self {
            yt_dlp: "yt-dlp".into(),
            ffmpeg: "ffmpeg".into(),
            ffprobe: "ffprobe".into(),
        }
    }
}

pub fn capture(command: &mut Command, purpose: &str) -> Result<Vec<u8>> {
    let result = command.output().with_context(|| {
        format!("Cannot start {purpose}; verify the executable is installed and on PATH")
    })?;
    if !result.status.success() {
        let stderr = String::from_utf8_lossy(&result.stderr);
        let tail: String = stderr
            .chars()
            .rev()
            .take(4000)
            .collect::<String>()
            .chars()
            .rev()
            .collect();
        bail!(
            "{purpose} failed ({}). No track was skipped.\n{}",
            result.status,
            tail.trim()
        );
    }
    Ok(result.stdout)
}

pub fn check_tool(executable: &OsString, version_flag: &str, installation: &str) -> Result<String> {
    let bytes = capture(
        Command::new(executable).arg(version_flag),
        &format!("dependency {}", executable.to_string_lossy()),
    )
    .with_context(|| {
        format!("Install {installation}, then retry. On macOS: brew install yt-dlp ffmpeg")
    })?;
    let line = String::from_utf8_lossy(&bytes)
        .lines()
        .next()
        .unwrap_or("")
        .to_owned();
    ensure!(
        !line.is_empty(),
        "{} returned no version; verify installation",
        executable.to_string_lossy()
    );
    Ok(line)
}
impl Tools {
    pub fn check(&self, local: bool) -> Result<BTreeMap<String, String>> {
        let mut versions = BTreeMap::new();
        if !local {
            versions.insert(
                "yt-dlp".into(),
                check_tool(
                    &self.yt_dlp,
                    "--version",
                    "yt-dlp (standalone executable supported)",
                )?,
            );
        }
        versions.insert(
            "ffmpeg".into(),
            check_tool(
                &self.ffmpeg,
                "-version",
                "FFmpeg with FLAC and libmp3lame encoders",
            )?,
        );
        versions.insert(
            "ffprobe".into(),
            check_tool(&self.ffprobe, "-version", "FFprobe (included with FFmpeg)")?,
        );
        Ok(versions)
    }
}

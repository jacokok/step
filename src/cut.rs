use crate::{
    SAMPLE_RATE, analysis,
    manifest::{Manifest, Track, hash_file},
    playlist,
    process::{Tools, capture},
};
use anyhow::{Context, Result, ensure};
use clap::Args;
use serde_json::json;
use std::{
    fs::{self, File, OpenOptions},
    io::{BufReader, Read},
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Debug, Args)]
pub struct CutArgs {
    /// State, PCM cache and original backups. Default: out/mix.work.
    #[arg(long, default_value = "out/mix.work")]
    pub work_dir: PathBuf,
    /// Index-prefixed MP3/WAV/FLAC files; defaults to <work-dir>/tracks.
    #[arg(long)]
    pub local_tracks: Option<PathBuf>,
    /// Silence threshold in dBFS. Higher (less negative) values trim more aggressively.
    #[arg(long, default_value_t = -50.0, allow_hyphen_values = true)]
    pub threshold: f64,
    /// Minimum continuous silence at each edge, in seconds.
    #[arg(long, default_value_t = 0.1)]
    pub min_silence: f64,
    /// Preview cuts without replacing source files (PCM caching is allowed).
    #[arg(long)]
    pub dry_run: bool,
}

pub fn run(args: CutArgs, tools: &Tools) -> Result<()> {
    ensure!(
        args.threshold.is_finite() && args.threshold < 0.0,
        "--threshold must be a finite negative dBFS value (for example, -50)"
    );
    ensure!(
        args.min_silence.is_finite() && args.min_silence > 0.0,
        "--min-silence must be a finite positive number of seconds"
    );
    let versions = tools.check(true)?;
    fs::create_dir_all(&args.work_dir)?;
    let work = fs::canonicalize(&args.work_dir)?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(work.join("workflow.lock"))?;
    lock.try_lock().context("Work directory is locked by another run; wait for it to finish or use a different --work-dir")?;
    let local = fs::canonicalize(args.local_tracks.as_deref().unwrap_or(&work.join("tracks")))
        .context("Cannot open tracks directory; run step get or supply --local-tracks")?;
    let path = work.join("manifest.json");
    let mut manifest = if path.exists() {
        Manifest::load(&path)?
    } else {
        Manifest::new(
            String::new(),
            format!("local:{}", local.display()),
            Vec::new(),
        )
    };
    let current = playlist::import_local(&local)?;
    manifest.tracks = current
        .into_iter()
        .map(|track| {
            manifest
                .tracks
                .iter()
                .find(|old| old.index == track.index && old.path == track.path)
                .cloned()
                .unwrap_or(track)
        })
        .collect();
    manifest.tools.extend(versions);
    manifest.last_error = None;
    if !args.dry_run {
        ensure!(
            manifest
                .config
                .entry("cut_history".into())
                .or_insert_with(|| json!([]))
                .is_array(),
            "Invalid cut history"
        );
    }
    let result = drive(&args, tools, &work, &path, &mut manifest);
    if !args.dry_run {
        if let Err(error) = &result {
            manifest.last_error = Some(format!("{error:#}"));
        }
        manifest.save(&path)?;
    }
    result?;
    println!(
        "{} Run step beat before mixing changed tracks.",
        if args.dry_run {
            "Cut preview complete; no source files changed."
        } else {
            "Cut complete."
        }
    );
    Ok(())
}

fn drive(
    args: &CutArgs,
    tools: &Tools,
    work: &Path,
    manifest_path: &Path,
    manifest: &mut Manifest,
) -> Result<()> {
    let version = manifest
        .tools
        .get("ffmpeg")
        .context("FFmpeg version missing")?
        .clone();
    for position in 0..manifest.tracks.len() {
        let mut track = manifest.tracks[position].clone();
        analysis::prepare_pcm(&mut track, &work.join("pcm"), tools, &version)?;
        let frames = track.frames.context("PCM frame count missing")?;
        let bounds = edge_bounds(
            track.pcm_path.as_deref().context("PCM missing")?,
            frames,
            args.threshold,
            args.min_silence,
        )?;
        let Some((start, end)) = bounds else {
            println!(
                "{:05}: entirely below threshold; left unchanged",
                track.index
            );
            continue;
        };
        if start == 0 && end == frames {
            println!("{:05}: no qualifying edge silence; unchanged", track.index);
            continue;
        }
        println!(
            "{:05}: trim {:.3}s from start, {:.3}s from end{}",
            track.index,
            start as f64 / SAMPLE_RATE as f64,
            (frames - end) as f64 / SAMPLE_RATE as f64,
            if args.dry_run { " (preview)" } else { "" }
        );
        if args.dry_run {
            continue;
        }
        let source_hash = track.sha256.as_deref().context("Source hash missing")?;
        let backup = backup_original(&track, source_hash, &work.join("originals"))?;
        replace_audio(tools, &track, start, end)?;
        let history = manifest
            .config
            .entry("cut_history".into())
            .or_insert_with(|| json!([]));
        history
            .as_array_mut()
            .context("Invalid cut history")?
            .push(json!({
                "index": track.index, "backup": backup, "original_sha256": source_hash,
                "threshold_dbfs": args.threshold, "min_silence_seconds": args.min_silence,
                "start_sample": start, "end_sample": end, "original_frames": frames,
            }));
        let mut changed = Track::new(track.index, track.id, track.title, track.url, track.path);
        changed.downloaded = true;
        manifest.tracks[position] = changed;
        manifest.transitions.clear();
        manifest.output_plan_key = None;
        manifest.save(manifest_path)?;
        println!("  Original saved: {}", backup.display());
    }
    Ok(())
}

/// Scan decoded stereo PCM using a per-sample peak threshold on both channels.
/// Internal silence is counted but never removed; all-silent files have no cut.
fn edge_bounds(
    path: &Path,
    frames: u64,
    threshold: f64,
    min_silence: f64,
) -> Result<Option<(u64, u64)>> {
    let amplitude = 10.0_f64.powf(threshold / 20.0);
    let minimum = (min_silence * SAMPLE_RATE as f64).ceil();
    let mut reader = BufReader::new(File::open(path)?);
    let mut leading = 0;
    let mut trailing = 0;
    let mut heard_audio = false;
    let mut frame = [0u8; 8];
    for _ in 0..frames {
        reader.read_exact(&mut frame)?;
        let left = f32::from_le_bytes(frame[..4].try_into().unwrap());
        let right = f32::from_le_bytes(frame[4..].try_into().unwrap());
        ensure!(
            left.is_finite() && right.is_finite(),
            "Audio contains non-finite samples"
        );
        if (left.abs() as f64) <= amplitude && (right.abs() as f64) <= amplitude {
            if !heard_audio {
                leading += 1;
            }
            trailing += 1;
        } else {
            heard_audio = true;
            trailing = 0;
        }
    }
    if !heard_audio {
        return Ok(None);
    }
    Ok(Some((
        if leading as f64 >= minimum {
            leading
        } else {
            0
        },
        frames
            - if trailing as f64 >= minimum {
                trailing
            } else {
                0
            },
    )))
}

fn backup_original(track: &Track, hash: &str, dir: &Path) -> Result<PathBuf> {
    fs::create_dir_all(dir)?;
    let ext = track
        .path
        .extension()
        .context("Track has no extension")?
        .to_string_lossy();
    let backup = dir.join(format!("{:05}-{hash}.{ext}", track.index));
    if backup.exists() {
        ensure!(
            hash_file(&backup)? == hash,
            "Original backup is corrupt: {}",
            backup.display()
        );
    } else {
        let partial = backup.with_extension("partial");
        fs::copy(&track.path, &partial)?;
        ensure!(
            hash_file(&partial)? == hash,
            "Source changed while backing it up; retry"
        );
        fs::rename(partial, &backup)?;
    }
    Ok(backup)
}

fn replace_audio(tools: &Tools, track: &Track, start: u64, end: u64) -> Result<()> {
    let ext = track
        .path
        .extension()
        .and_then(|v| v.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let codec: &[&str] = match ext.as_str() {
        "mp3" => &["-c:a", "libmp3lame", "-q:a", "0"],
        "flac" => &["-c:a", "flac"],
        "wav" => &["-c:a", "pcm_f32le"],
        _ => anyhow::bail!("Unsupported track format"),
    };
    let temporary = track.path.with_file_name(format!(
        ".{}.step-cut.partial",
        track
            .path
            .file_name()
            .context("Track has no filename")?
            .to_string_lossy()
    ));
    // Beside the source so replacement is atomic even with external --local-tracks.
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .with_context(|| {
            format!(
                "Cannot reserve {}; remove it if left by an interrupted cut",
                temporary.display()
            )
        })?;
    let result = (|| -> Result<()> {
        let mut command = Command::new(&tools.ffmpeg);
        command
            .args([
                "-nostdin",
                "-hide_banner",
                "-loglevel",
                "error",
                "-xerror",
                "-y",
                "-f",
                "f32le",
                "-ar",
                "48000",
                "-ac",
                "2",
                "-i",
            ])
            .arg(track.pcm_path.as_deref().context("PCM missing")?)
            .arg("-af")
            .arg(format!(
                "atrim=start_sample={start}:end_sample={end},asetpts=PTS-STARTPTS"
            ))
            .args(codec)
            .arg("-f")
            .arg(&ext)
            .arg(&temporary);
        capture(
            &mut command,
            &format!("cut encode of track {}", track.index),
        )?;
        let mut probe = Command::new(&tools.ffprobe);
        probe
            .args([
                "-v",
                "error",
                "-select_streams",
                "a:0",
                "-show_entries",
                "stream=codec_name",
                "-of",
                "default=nw=1:nk=1",
            ])
            .arg(&temporary);
        ensure!(
            !capture(&mut probe, "cut output validation")?.is_empty(),
            "Cut output has no audio stream"
        );
        ensure!(
            hash_file(&track.path)? == track.sha256.as_deref().context("Source hash missing")?,
            "Source changed during cut; refusing to replace it"
        );
        fs::set_permissions(&temporary, fs::metadata(&track.path)?.permissions())?;
        fs::rename(&temporary, &track.path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

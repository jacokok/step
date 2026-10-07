use crate::{
    SAMPLE_RATE, analysis,
    manifest::{Manifest, hash_bytes, hash_file},
    models, playlist,
    process::Tools,
    render::{self, Format},
    transition::{self, Options, Overrides},
};
use anyhow::{Context, Result, bail, ensure};
use clap::Args;
use serde_json::json;
use std::{
    fs::{self, OpenOptions},
    path::{Path, PathBuf},
};

#[derive(Debug, Args)]
pub struct MixArgs {
    /// YouTube Music or YouTube playlist URL (quote it in your shell).
    pub playlist_url: String,
    /// Final output file. Default: out/mix.mp3.
    pub output: Option<PathBuf>,
    /// State, downloads and PCM cache. Default: <output-stem>.work beside output.
    #[arg(long)]
    pub work_dir: Option<PathBuf>,
    /// Directory containing checksum-pinned ONNX model files.
    #[arg(long, default_value = "models")]
    pub model_dir: PathBuf,
    /// Use the smaller, less accurate model.
    #[arg(long)]
    pub small_model: bool,
    /// Analyze and report; download/cache work is allowed, but no mix is created.
    #[arg(long)]
    pub dry_run: bool,
    #[arg(long, default_value_t = 1.0)]
    pub overlap: f64,
    /// Maximum discarded outgoing tail, in seconds.
    #[arg(long, default_value_t = 12.0)]
    pub tail_window: f64,
    /// Maximum discarded incoming intro, in seconds.
    #[arg(long, default_value_t = 12.0)]
    pub intro_window: f64,
    /// Minimum uncalibrated beat-model activation accepted for automatic transitions.
    #[arg(long, default_value_t = 0.5)]
    pub min_beat_score: f64,
    /// JSON map of adjacent index pairs to explicit beat timestamps.
    #[arg(long)]
    pub overrides: Option<PathBuf>,
    /// Netscape-format yt-dlp cookies file, used for metadata and every track download.
    #[arg(long)]
    pub cookies: Option<PathBuf>,
    /// Final encoding format (defaults to MP3, regardless of a supplied filename).
    #[arg(long, value_enum, default_value = "mp3")]
    pub format: Format,
    /// Replace an existing output after a successful encode.
    #[arg(long)]
    pub force: bool,
    /// Offline import of index-prefixed MP3/WAV/FLAC files instead of YouTube downloads.
    #[arg(long)]
    pub local_tracks: Option<PathBuf>,
}

impl MixArgs {
    pub fn output_path(&self) -> PathBuf {
        self.output.clone().unwrap_or_else(|| {
            PathBuf::from("out").join(format!("mix.{}", self.format.extension()))
        })
    }
}

fn absolute(path: &Path) -> Result<PathBuf> {
    Ok(if path.is_absolute() {
        path.into()
    } else {
        std::env::current_dir()?.join(path)
    })
}

pub fn run(args: MixArgs, tools: &Tools) -> Result<()> {
    playlist::validate_url(&args.playlist_url)?;
    let options = Options {
        overlap: args.overlap,
        tail_window: args.tail_window,
        intro_window: args.intro_window,
        min_beat_score: args.min_beat_score,
    };
    options.validate()?;
    let output_path = args.output_path();
    let format = Format::infer(&output_path, Some(args.format))?;
    let versions = tools.check(args.local_tracks.is_some())?;
    let stamp = models::verify(&args.model_dir, args.small_model)?;
    let overrides: Overrides = match &args.overrides {
        Some(path) => serde_json::from_slice(&fs::read(path).with_context(|| format!("Cannot read overrides {}", path.display()))?)
            .context("Invalid overrides JSON; expected keys like 1->2 with outgoing_beat_seconds, incoming_beat_seconds, and optional overlap_seconds")?,
        None => Overrides::new(),
    };
    let requested_output = absolute(&output_path)?;
    fs::create_dir_all(
        requested_output
            .parent()
            .context("Output needs a parent directory")?,
    )?;
    let output = fs::canonicalize(requested_output.parent().unwrap())?.join(
        requested_output
            .file_name()
            .context("Output needs a filename")?,
    );
    let default_work = output.with_file_name(format!(
        "{}.work",
        output.file_stem().unwrap().to_string_lossy()
    ));
    let work = absolute(args.work_dir.as_deref().unwrap_or(&default_work))?;
    fs::create_dir_all(&work)?;
    let work = fs::canonicalize(work)?;
    let _lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(work.join("workflow.lock"))?;
    _lock.try_lock().context("Work directory is locked by another run; wait for it to finish or use a different --work-dir")?;
    let manifest_path = work.join("manifest.json");
    let track_dir = work.join("tracks");
    let local = args
        .local_tracks
        .as_ref()
        .map(fs::canonicalize)
        .transpose()
        .context("Cannot open local tracks directory")?;
    ensure!(
        !local
            .as_ref()
            .is_some_and(|p| output.parent() == Some(p.as_path())),
        "Place the output outside the --local-tracks directory so resume does not import the generated mix"
    );
    let source = local
        .as_ref()
        .map(|p| format!("local:{}", p.display()))
        .unwrap_or("youtube".into());
    let mut manifest = if manifest_path.exists() {
        let mut value = Manifest::load(&manifest_path)?;
        ensure!(
            value.playlist_url == args.playlist_url && value.source == source,
            "Work directory belongs to another playlist/source; use a new --work-dir"
        );
        playlist::sort_tracks(&mut value.tracks)?;
        if let Some(dir) = &local {
            let current = playlist::import_local(dir)?;
            ensure!(
                current.len() == value.tracks.len()
                    && current
                        .iter()
                        .zip(&value.tracks)
                        .all(|(a, b)| a.index == b.index && a.path == b.path),
                "Local track list changed; use a new work directory rather than silently adding/removing tracks"
            );
        }
        value
    } else {
        fs::create_dir_all(&track_dir)?;
        let tracks = if let Some(dir) = &local {
            playlist::import_local(dir)?
        } else {
            println!("Snapshotting playlist order...");
            playlist::snapshot(
                tools,
                &args.playlist_url,
                &track_dir,
                args.cookies.as_deref(),
            )?
        };
        Manifest::new(args.playlist_url.clone(), source, tracks)
    };
    ensure!(
        output != manifest_path && output != work.join("workflow.lock"),
        "Output cannot overwrite workflow state"
    );
    for track in &manifest.tracks {
        ensure!(
            track.path != output && !track.pcm_path.as_ref().is_some_and(|p| p == &output),
            "Output cannot overwrite a source/cache track"
        );
        if output.exists() {
            ensure!(
                fs::canonicalize(&output)?
                    != fs::canonicalize(&track.path).unwrap_or(track.path.clone()),
                "Output resolves to a source track"
            );
        }
    }
    manifest.tools = versions;
    manifest
        .config
        .insert("transition_options".into(), serde_json::to_value(&options)?);
    manifest
        .config
        .insert("overrides".into(), serde_json::to_value(&overrides)?);
    manifest.config.insert("detector".into(), json!(stamp));
    manifest
        .config
        .insert("sample_rate".into(), json!(SAMPLE_RATE));
    manifest.config.insert("format".into(), json!(format));
    manifest
        .config
        .insert("headroom_db".into(), json!(-3.0102999566));
    manifest.config.insert(
        "selection".into(),
        json!(
            "greedy adjacent pairs; minimum total trim, then tail trim, then beat sample positions"
        ),
    );
    manifest.warnings = vec!["Beat detection is an estimate (approximately 20 ms model frame grid). Aligning one beat does not synchronize different BPMs throughout an overlap. No tempo/pitch/speed adjustment is performed.".into()];
    if local.is_some() {
        manifest.warnings.push("Offline local import: no YouTube access or download performed; playlist URL is provenance only".into());
    } else {
        manifest.warnings.push("Playlist order is an immutable metadata snapshot. Remote changes are not refreshed on resume; use a new work directory to refresh.".into());
    }
    manifest.last_error = None;
    manifest.transitions.clear();
    manifest.save(&manifest_path)?;
    let result = drive(
        &args,
        tools,
        &work,
        &output,
        format,
        &options,
        &overrides,
        &stamp,
        &manifest_path,
        &mut manifest,
    );
    if let Err(error) = &result {
        manifest.last_error = Some(format!("{error:#}"));
        // Preserve completed tracks and transitions even when a later stage fails.
        if let Err(save_error) = manifest.save(&manifest_path) {
            eprintln!("Also failed to save state: {save_error:#}");
        }
        report(&manifest, &manifest_path);
    }
    result
}

#[allow(clippy::too_many_arguments)]
fn drive(
    args: &MixArgs,
    tools: &Tools,
    work: &Path,
    output: &Path,
    format: Format,
    options: &Options,
    overrides: &Overrides,
    stamp: &str,
    manifest_path: &Path,
    manifest: &mut Manifest,
) -> Result<()> {
    let mut detector = None;
    let ffmpeg_version = manifest
        .tools
        .get("ffmpeg")
        .context("FFmpeg version missing")?
        .clone();
    for position in 0..manifest.tracks.len() {
        let track = &mut manifest.tracks[position];
        println!("Preparing {:05}: {}", track.index, track.title);
        if !track.downloaded || !track.path.is_file() {
            if args.local_tracks.is_some() {
                bail!(
                    "Local track missing at index {}: {}",
                    track.index,
                    track.path.display()
                );
            }
            playlist::download(tools, track, &work.join("staging"), args.cookies.as_deref())?;
            track.downloaded = true;
            manifest.save(manifest_path)?;
        }
        let track = &mut manifest.tracks[position];
        let changed = analysis::prepare_pcm(track, &work.join("pcm"), tools, &ffmpeg_version)?;
        if changed {
            println!("  decoded PCM; analysis invalidated");
        }
        let key = analysis::cache_key(track, stamp)?;
        let cached = track.analysis.as_ref().is_some_and(|a| a.cache_key == key);
        manifest.save(manifest_path)?;
        if cached {
            println!("  using cached beat analysis");
        } else {
            if detector.is_none() {
                detector = Some(analysis::load_detector(&args.model_dir, args.small_model)?);
            }
            println!("  detecting beats...");
            analysis::analyze(
                &mut manifest.tracks[position],
                detector.as_mut().unwrap(),
                tools,
                stamp,
            )?;
            manifest.save(manifest_path)?;
        }
    }
    let keys: Vec<String> = manifest
        .tracks
        .windows(2)
        .map(|p| format!("{}->{}", p[0].index, p[1].index))
        .collect();
    for key in overrides.keys() {
        ensure!(
            keys.contains(key),
            "Override {key:?} does not name an adjacent playlist pair"
        );
    }
    for (position, key) in keys.iter().enumerate() {
        let (start, n) = manifest
            .transitions
            .last()
            .map(|t| (t.incoming_start_sample, t.overlap_samples))
            .unwrap_or((0, 0));
        let selected = transition::select(
            &manifest.tracks[position],
            &manifest.tracks[position + 1],
            options,
            overrides.get(key),
            start,
            n,
        )?;
        manifest.transitions.push(selected);
        manifest.save(manifest_path)?;
    }
    let graph_path = work.join("filtergraph.txt");
    fs::write(
        &graph_path,
        render::filter_graph(&manifest.tracks, &manifest.transitions)?,
    )?;
    report(manifest, manifest_path);
    if args.dry_run {
        println!(
            "Dry run complete: no mix created. Filter graph: {}",
            graph_path.display()
        );
        return Ok(());
    }
    let plan_key = hash_bytes(&serde_json::to_vec(&json!({
        "pcm": manifest.tracks.iter().map(|t| &t.pcm_sha256).collect::<Vec<_>>(),
        "transitions": manifest.transitions, "format": format, "graph": fs::read_to_string(&graph_path)?,
        "ffmpeg": ffmpeg_version, "encoder": "v1"
    }))?);
    if manifest.output.as_deref() == Some(output)
        && manifest.output_plan_key.as_deref() == Some(&plan_key)
        && output.is_file()
        && manifest.output_sha256.as_deref() == Some(&hash_file(output)?)
    {
        println!("Unchanged mix already exists: {}", output.display());
        return Ok(());
    }
    render::render(
        tools,
        &manifest.tracks,
        &manifest.transitions,
        output,
        &graph_path,
        format,
        args.force,
    )?;
    manifest.output = Some(output.into());
    manifest.output_sha256 = Some(hash_file(output)?);
    manifest.output_plan_key = Some(plan_key);
    manifest.save(manifest_path)?;
    Ok(())
}

pub fn report(manifest: &Manifest, path: &Path) {
    println!("\nPlaylist order (explicit indices):");
    for track in &manifest.tracks {
        if let Some(a) = &track.analysis {
            println!(
                "  {:05} {} | {:.3}s | BPM {} | confidence {:.3} (uncalibrated) | regularity {:.3} | {} beats",
                track.index,
                track.title,
                track.duration.unwrap_or(0.0),
                a.bpm.map(|v| format!("{v:.2}")).unwrap_or("unknown".into()),
                a.confidence,
                a.interval_regularity,
                a.beats.len()
            );
            for warning in &a.warnings {
                println!("    WARNING: {warning}");
            }
        } else {
            println!("  {:05} {} | analysis incomplete", track.index, track.title);
        }
    }
    println!("Transitions:");
    for t in &manifest.transitions {
        println!(
            "  {}->{}: outgoing beat {:.6}s = incoming beat {:.6}s; overlap {:.6}s; tail trim {:.3}s, intro trim {:.3}s [{}]",
            t.outgoing_index,
            t.incoming_index,
            t.outgoing_beat_seconds,
            t.incoming_beat_seconds,
            t.overlap_seconds,
            t.tail_trim_seconds,
            t.intro_trim_seconds,
            t.source
        );
        for warning in &t.warnings {
            println!("    WARNING: {warning}");
        }
    }
    if manifest.transitions.len() + 1 == manifest.tracks.len()
        && let Ok(frames) = render::expected_frames(&manifest.tracks, &manifest.transitions)
    {
        println!(
            "Planned mix duration: {:.3}s",
            frames as f64 / SAMPLE_RATE as f64
        );
    }
    for warning in &manifest.warnings {
        println!("WARNING: {warning}");
    }
    if let Some(error) = &manifest.last_error {
        println!("ERROR: {error}");
    }
    println!("Manifest: {}", path.display());
}

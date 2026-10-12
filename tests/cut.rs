use std::{fs, path::Path, process::Command};
use step::{
    SAMPLE_RATE, analysis,
    manifest::{Analysis, Manifest, hash_file},
    playlist,
    process::Tools,
};

fn wav(path: &Path, segments: &[(usize, f32, f32)]) {
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate: SAMPLE_RATE,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut writer = hound::WavWriter::create(path, spec).unwrap();
    for &(frames, left, right) in segments {
        for _ in 0..frames {
            writer.write_sample(left).unwrap();
            writer.write_sample(right).unwrap();
        }
    }
    writer.finalize().unwrap();
}

fn invoke(work: &Path, extra: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_step"))
        .args(["cut", "--work-dir"])
        .arg(work)
        .args(extra)
        .env("STEP_MODEL_DIR", work.join("missing-models"))
        .output()
        .unwrap()
}

fn samples(path: &Path) -> Vec<f32> {
    hound::WavReader::open(path)
        .unwrap()
        .samples::<f32>()
        .map(Result::unwrap)
        .collect()
}

#[test]
fn cut_trims_only_edges_backs_up_original_and_invalidates_beats() {
    let dir = tempfile::tempdir().unwrap();
    let work = dir.path().join("work");
    let tracks = work.join("tracks");
    fs::create_dir_all(&tracks).unwrap();
    let source = tracks.join("00001-song.wav");
    wav(
        &source,
        &[
            (9600, 0.0, 0.0),
            (4800, 0.25, 0.25),
            (9600, 0.0, 0.0),
            (4800, 0.25, 0.25),
            (14400, 0.0, 0.0),
        ],
    );
    let original = fs::read(&source).unwrap();
    let mut imported = playlist::import_local(&tracks).unwrap();
    let tools = Tools::default();
    let versions = tools.check(true).unwrap();
    analysis::prepare_pcm(
        &mut imported[0],
        &work.join("pcm"),
        &tools,
        &versions["ffmpeg"],
    )
    .unwrap();
    imported[0].analysis = Some(Analysis {
        cache_key: "fixture".into(),
        detector: "fixture".into(),
        bpm: None,
        confidence: 1.0,
        confidence_kind: "fixture".into(),
        interval_regularity: 1.0,
        beats: vec![],
        downbeats: vec![],
        warnings: vec![],
    });
    let mut manifest = Manifest::new(
        String::new(),
        format!("local:{}", tracks.display()),
        imported,
    );
    manifest
        .config
        .insert("detector".into(), serde_json::json!("fixture"));
    manifest.output_plan_key = Some("old-plan".into());
    manifest.save(&work.join("manifest.json")).unwrap();
    let state_before_preview = fs::read(work.join("manifest.json")).unwrap();
    assert!(invoke(&work, &["--dry-run"]).status.success());
    assert_eq!(fs::read(&source).unwrap(), original);
    assert_eq!(
        fs::read(work.join("manifest.json")).unwrap(),
        state_before_preview
    );
    let result = invoke(&work, &["--threshold", "-50"]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let trimmed = samples(&source);
    assert_eq!(trimmed.len(), 19200 * 2);
    assert!(trimmed[..9600].iter().all(|&v| v == 0.25));
    assert!(trimmed[9600..28800].iter().all(|&v| v == 0.0));
    assert!(trimmed[28800..].iter().all(|&v| v == 0.25));
    let saved = Manifest::load(&work.join("manifest.json")).unwrap();
    assert_eq!(saved.tracks[0].path, source);
    assert!(saved.tracks[0].analysis.is_none());
    assert!(saved.tracks[0].pcm_path.is_none());
    assert!(saved.tracks[0].downloaded);
    assert!(saved.transitions.is_empty());
    assert!(saved.output_plan_key.is_none());
    let history = saved.config["cut_history"].as_array().unwrap();
    assert_eq!(history.len(), 1);
    assert_eq!(history[0]["start_sample"], 9600);
    assert_eq!(history[0]["end_sample"], 28800);
    let backup = Path::new(history[0]["backup"].as_str().unwrap());
    assert_eq!(fs::read(backup).unwrap(), original);
    let mix = Command::new(env!("CARGO_BIN_EXE_step"))
        .args(["mix", "--work-dir"])
        .arg(&work)
        .output()
        .unwrap();
    assert!(!mix.status.success());
    assert!(String::from_utf8_lossy(&mix.stderr).contains("run step beat first"));
    let hash = hash_file(&source).unwrap();
    assert!(invoke(&work, &[]).status.success());
    assert_eq!(hash_file(&source).unwrap(), hash);
    assert_eq!(
        Manifest::load(&work.join("manifest.json")).unwrap().config["cut_history"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn threshold_preview_minimum_duration_and_all_silent_safety() {
    let dir = tempfile::tempdir().unwrap();
    let work = dir.path().join("work");
    let tracks = work.join("tracks");
    fs::create_dir_all(&tracks).unwrap();
    let quiet = tracks.join("00001-quiet.wav");
    wav(
        &quiet,
        &[
            (9600, 0.001, 0.001),
            (4800, 0.25, 0.25),
            (9600, 0.001, 0.001),
        ],
    );
    let silent = tracks.join("00002-silent.wav");
    wav(&silent, &[(24000, 0.0, 0.0)]);
    let short = tracks.join("00003-short.wav");
    wav(
        &short,
        &[(2400, 0.0, 0.0), (4800, 0.25, 0.25), (2400, 0.0, 0.0)],
    );
    let stereo = tracks.join("00004-stereo.wav");
    wav(
        &stereo,
        &[(9600, 0.0, 0.25), (4800, 0.25, 0.25), (9600, 0.25, 0.0)],
    );
    let hashes: Vec<_> = [&quiet, &silent, &short, &stereo]
        .iter()
        .map(|p| hash_file(p).unwrap())
        .collect();
    let preview = invoke(&work, &["--threshold=-40", "--dry-run"]);
    assert!(
        preview.status.success(),
        "{}",
        String::from_utf8_lossy(&preview.stderr)
    );
    assert!(String::from_utf8_lossy(&preview.stdout).contains("trim 0.200s"));
    assert!(!work.join("originals").exists());
    assert!(!work.join("manifest.json").exists());
    assert!(invoke(&work, &["--threshold", "-80"]).status.success());
    for (path, hash) in [&quiet, &silent, &short, &stereo].iter().zip(&hashes) {
        assert_eq!(hash_file(path).unwrap(), *hash);
    }
    assert!(
        invoke(&work, &["--threshold", "-40", "--min-silence", "0.25"])
            .status
            .success()
    );
    assert_eq!(hash_file(&quiet).unwrap(), hashes[0]);
    assert!(invoke(&work, &["--threshold", "-40"]).status.success());
    assert_eq!(samples(&quiet).len(), 4800 * 2);
    for (path, hash) in [&silent, &short, &stereo].iter().zip(&hashes[1..]) {
        assert_eq!(hash_file(path).unwrap(), *hash);
    }
}

#[test]
fn cut_supports_external_tracks_and_mp3_and_flac() {
    let dir = tempfile::tempdir().unwrap();
    let work = dir.path().join("session");
    let tracks = dir.path().join("custom");
    fs::create_dir_all(&tracks).unwrap();
    let input = dir.path().join("input.wav");
    wav(
        &input,
        &[(9600, 0.0, 0.0), (24000, 0.25, 0.25), (9600, 0.0, 0.0)],
    );
    for (index, format) in [(1, "mp3"), (2, "flac")] {
        let source = tracks.join(format!("{index:05}-song.{format}"));
        assert!(
            Command::new("ffmpeg")
                .args(["-v", "error", "-i"])
                .arg(&input)
                .arg(&source)
                .status()
                .unwrap()
                .success()
        );
    }
    let result = Command::new(env!("CARGO_BIN_EXE_step"))
        .args(["cut", "--work-dir"])
        .arg(&work)
        .arg("--local-tracks")
        .arg(&tracks)
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let mut saved = Manifest::load(&work.join("manifest.json")).unwrap();
    assert_eq!(saved.config["cut_history"].as_array().unwrap().len(), 2);
    let tools = Tools::default();
    let versions = tools.check(true).unwrap();
    for track in &mut saved.tracks {
        analysis::prepare_pcm(track, &work.join("pcm"), &tools, &versions["ffmpeg"]).unwrap();
        assert!((track.duration.unwrap() - 0.5).abs() < 0.01);
    }
}

#[test]
fn invalid_cut_parameters_are_rejected() {
    let dir = tempfile::tempdir().unwrap();
    for extra in [
        ["--threshold", "NaN"],
        ["--threshold", "0"],
        ["--threshold", "2"],
        ["--min-silence", "0"],
        ["--min-silence", "inf"],
    ] {
        assert!(!invoke(dir.path(), &extra).status.success());
    }
}

#[cfg(unix)]
#[test]
fn failed_encode_keeps_source_and_backup() {
    use std::os::unix::fs::PermissionsExt;
    use step::cut::{self, CutArgs};
    let dir = tempfile::tempdir().unwrap();
    let work = dir.path().join("work");
    let tracks = work.join("tracks");
    fs::create_dir_all(&tracks).unwrap();
    let source = tracks.join("00001-song.wav");
    wav(
        &source,
        &[(9600, 0.0, 0.0), (4800, 0.25, 0.25), (9600, 0.0, 0.0)],
    );
    let hash = hash_file(&source).unwrap();
    let encoder = dir.path().join("encoder");
    fs::write(
        &encoder,
        r#"#!/bin/sh
for arg in "$@"; do
  case "$arg" in
    *.step-cut.partial) echo 'simulated encode failure' >&2; exit 42 ;;
  esac
done
exec ffmpeg "$@"
"#,
    )
    .unwrap();
    fs::set_permissions(&encoder, fs::Permissions::from_mode(0o755)).unwrap();
    let tools = Tools {
        ffmpeg: encoder.into_os_string(),
        yt_dlp: "missing-yt-dlp".into(),
        ..Tools::default()
    };
    let error = cut::run(
        CutArgs {
            work_dir: work.clone(),
            local_tracks: None,
            threshold: -50.0,
            min_silence: 0.1,
            dry_run: false,
        },
        &tools,
    )
    .unwrap_err();
    assert!(format!("{error:#}").contains("simulated encode failure"));
    assert_eq!(hash_file(&source).unwrap(), hash);
    let backup = fs::read_dir(work.join("originals"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    assert_eq!(hash_file(&backup).unwrap(), hash);
    assert_eq!(fs::read_dir(&tracks).unwrap().count(), 1);
    assert!(
        Manifest::load(&work.join("manifest.json"))
            .unwrap()
            .last_error
            .unwrap()
            .contains("simulated encode failure")
    );
}

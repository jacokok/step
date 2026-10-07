//! Optional full CLI test: real models/MP3/FFmpeg, fake YouTube transport.
//! Fetch small models first, then: cargo test --locked --test cli -- --ignored
#[cfg(unix)]
#[test]
#[ignore = "requires checksum-pinned small models and FFmpeg; see README"]
fn cli_download_failure_resume_dry_run_render_and_output_reuse() {
    use playlist_mix::{manifest::Manifest, models};
    use std::{
        fs,
        os::unix::fs::PermissionsExt,
        path::PathBuf,
        process::{Command, Output},
    };
    let project = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let model_dir = std::env::var_os("PLAYLIST_MIX_TEST_MODEL_DIR")
        .map(PathBuf::from)
        .unwrap_or(project.join("models"));
    models::verify(&model_dir, true).expect("run `cargo run -- models --small` first");
    let dir = tempfile::tempdir().unwrap();
    let wav_path = dir.path().join("source.wav");
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: 48000,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut wav = hound::WavWriter::create(&wav_path, spec).unwrap();
    for frame in 0..192000 {
        let phase = (frame % 24000) as f64 / 48000.0;
        let sample = (0.5
            * (-phase * 30.0).exp()
            * (phase * std::f64::consts::TAU * 80.0).sin()
            * i16::MAX as f64) as i16;
        wav.write_sample(sample).unwrap();
    }
    wav.finalize().unwrap();
    let mp3 = dir.path().join("source.mp3");
    assert!(
        Command::new("ffmpeg")
            .args(["-v", "error", "-i"])
            .arg(&wav_path)
            .args(["-c:a", "libmp3lame", "-q:a", "0"])
            .arg(&mp3)
            .status()
            .unwrap()
            .success()
    );
    let bin_dir = dir.path().join("bin");
    fs::create_dir(&bin_dir).unwrap();
    let fake = bin_dir.join("yt-dlp");
    fs::write(&fake, r#"#!/bin/sh
metadata=no
template=
url=
while [ "$#" -gt 0 ]; do
  case "$1" in
    --version) echo 'fake-transport-1'; exit 0 ;;
    --dump-single-json) metadata=yes ;;
    --output) shift; template=$1 ;;
    https://*) url=$1 ;;
  esac
  shift
done
if [ "$metadata" = yes ]; then
  echo metadata >> "$REQUEST_LOG"
  printf '%s\n' '{"playlist_count":2,"entries":[{"playlist_index":2,"id":"second","title":"Second"},{"playlist_index":1,"id":"first","title":"First"}]}'
  exit 0
fi
printf 'download %s\n' "$url" >> "$REQUEST_LOG"
case "$url" in
  *second*) if [ "${FAIL_SECOND:-}" = yes ]; then echo 'simulated access denial' >&2; exit 42; fi ;;
esac
output=${template%'%(ext)s'}mp3
cp "$SOURCE_MP3" "$output"
"#).unwrap();
    fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
    let log = dir.path().join("requests.txt");
    let work = dir.path().join("work");
    let output = dir.path().join("out/mix.mp3");
    let overrides = dir.path().join("overrides.json");
    fs::write(
        &overrides,
        r#"{"1->2":{"outgoing_beat_seconds":3.5,"incoming_beat_seconds":0.5}}"#,
    )
    .unwrap();
    let mut paths = vec![bin_dir];
    paths.extend(std::env::split_paths(&std::env::var_os("PATH").unwrap()));
    let path = std::env::join_paths(paths).unwrap();
    let invoke = |dry: bool, fail: bool| -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_playlist-mix"));
        command
            .current_dir(dir.path())
            .args([
                "mix",
                "https://music.youtube.com/playlist?list=TEST_SUPPLIED_AT_RUNTIME",
            ])
            .arg("--work-dir")
            .arg(&work)
            .arg("--model-dir")
            .arg(&model_dir)
            .arg("--small-model")
            .arg("--overrides")
            .arg(&overrides)
            .env("PATH", &path)
            .env("SOURCE_MP3", &mp3)
            .env("REQUEST_LOG", &log);
        if dry {
            command.arg("--dry-run");
        }
        if fail {
            command.env("FAIL_SECOND", "yes");
        }
        command.output().unwrap()
    };
    let failed = invoke(true, true);
    assert!(!failed.status.success());
    let saved = Manifest::load(&work.join("manifest.json")).unwrap();
    assert_eq!(
        saved.tracks.iter().map(|t| t.index).collect::<Vec<_>>(),
        vec![1, 2]
    );
    assert!(saved.tracks[0].downloaded && !saved.tracks[1].downloaded);
    assert!(
        saved
            .last_error
            .unwrap()
            .contains("simulated access denial")
    );
    assert!(!output.exists());
    let first_analysis = serde_json::to_value(&saved.tracks[0].analysis).unwrap();
    let dry = invoke(true, false);
    assert!(
        dry.status.success(),
        "{}",
        String::from_utf8_lossy(&dry.stderr)
    );
    assert!(!output.exists());
    let saved = Manifest::load(&work.join("manifest.json")).unwrap();
    assert!(saved.last_error.is_none());
    assert_eq!(
        first_analysis,
        serde_json::to_value(&saved.tracks[0].analysis).unwrap()
    );
    assert_eq!(saved.transitions[0].source, "user_override");
    let rendered = invoke(false, false);
    assert!(
        rendered.status.success(),
        "{}",
        String::from_utf8_lossy(&rendered.stderr)
    );
    let saved = Manifest::load(&work.join("manifest.json")).unwrap();
    let output_hash = saved.output_sha256.unwrap();
    assert!(output.exists());
    let resumed = invoke(false, false);
    assert!(
        resumed.status.success(),
        "{}",
        String::from_utf8_lossy(&resumed.stderr)
    );
    assert!(String::from_utf8_lossy(&resumed.stdout).contains("Unchanged mix already exists"));
    assert_eq!(
        Manifest::load(&work.join("manifest.json"))
            .unwrap()
            .output_sha256
            .unwrap(),
        output_hash
    );
    let requests = fs::read_to_string(log).unwrap();
    assert_eq!(requests.lines().filter(|x| *x == "metadata").count(), 1);
    assert_eq!(
        requests
            .lines()
            .filter(|x| x.contains("watch?v=first"))
            .count(),
        1
    );
    assert_eq!(
        requests
            .lines()
            .filter(|x| x.contains("watch?v=second"))
            .count(),
        2
    ); // failed attempt + resume
}

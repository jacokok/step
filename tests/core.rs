use playlist_mix::{
    SAMPLE_RATE, analysis,
    manifest::{Analysis, Beat, Manifest, Track, hash_file},
    models, playlist,
    process::{Tools, check_tool},
    render::{self, Format},
    transition::{self, Options, Override, Overrides},
};
use serde_json::json;
use std::{fs, path::Path, process::Command};

fn track(index: u32, duration: f64, times: &[f64]) -> Track {
    let mut track = Track::new(
        index,
        format!("id{index}"),
        format!("track{index}"),
        String::new(),
        format!("{index:05}.mp3").into(),
    );
    track.frames = Some((duration * SAMPLE_RATE as f64).round() as u64);
    track.duration = Some(duration);
    track.analysis = Some(Analysis {
        cache_key: "fixture".into(),
        detector: "fixture".into(),
        bpm: Some(120.0),
        confidence: 0.9,
        confidence_kind: "fixture".into(),
        interval_regularity: 1.0,
        beats: times
            .iter()
            .map(|t| Beat {
                seconds: *t,
                sample: (*t * SAMPLE_RATE as f64).round() as u64,
                score: 0.9,
            })
            .collect(),
        downbeats: vec![],
        warnings: vec![],
    });
    track
}

#[test]
fn numeric_playlist_index_sorting_and_duplicate_errors() {
    let mut tracks = vec![
        track(10, 10.0, &[]),
        track(2, 10.0, &[]),
        track(1, 10.0, &[]),
    ];
    playlist::sort_tracks(&mut tracks).unwrap();
    assert_eq!(
        tracks.iter().map(|t| t.index).collect::<Vec<_>>(),
        vec![1, 2, 10]
    );
    tracks.push(track(2, 10.0, &[]));
    assert!(
        playlist::sort_tracks(&mut tracks)
            .unwrap_err()
            .to_string()
            .contains("Duplicate")
    );
}

#[test]
fn metadata_order_is_snapshotted_and_unavailable_tracks_are_not_skipped() {
    let data = json!({"entries": [{"playlist_index": 2, "id": "second"}, {"playlist_index": 1, "id": "first"}]});
    let tracks = playlist::parse_playlist(&data, Path::new("tracks")).unwrap();
    assert_eq!(tracks[0].index, 1);
    assert!(tracks[0].path.ends_with("00001-first.mp3"));
    let fallback = playlist::parse_playlist(
        &json!({"entries": [{"id":"first"},{"id":"second"}]}),
        Path::new("tracks"),
    )
    .unwrap();
    assert_eq!(fallback[1].index, 2);
    assert!(
        playlist::parse_playlist(
            &json!({"entries": [{"id":"first"},null]}),
            Path::new("tracks")
        )
        .is_err()
    );
    assert!(
        playlist::parse_playlist(
            &json!({"entries": [{"id":"first","playlist_index":2}]}),
            Path::new("tracks")
        )
        .is_err()
    );
    assert!(playlist::parse_playlist(&json!({"entries": []}), Path::new("tracks")).is_err());
}

#[test]
fn local_import_uses_indices_not_filesystem_or_lexical_order() {
    let dir = tempfile::tempdir().unwrap();
    for name in ["10-ten.mp3", "2-two.mp3", "1-one.mp3"] {
        fs::write(dir.path().join(name), b"test").unwrap();
    }
    assert_eq!(
        playlist::import_local(dir.path())
            .unwrap()
            .iter()
            .map(|t| t.index)
            .collect::<Vec<_>>(),
        vec![1, 2, 10]
    );
    fs::write(dir.path().join("unordered.wav"), b"test").unwrap();
    assert!(
        playlist::import_local(dir.path())
            .unwrap_err()
            .to_string()
            .contains("numeric playlist-index")
    );
}

#[test]
fn transition_minimizes_trimming_and_is_order_independent_on_ties() {
    let mut a = track(1, 10.0, &[8.5, 9.5, 9.5]);
    let mut b = track(2, 10.0, &[1.0, 0.5, 0.5]);
    let first = transition::select(&a, &b, &Options::default(), None, 0, 0).unwrap();
    assert_eq!(
        (first.outgoing_beat_sample, first.incoming_beat_sample),
        (456000, 24000)
    );
    assert_eq!(first.tail_trim_seconds, 0.0);
    assert_eq!(first.intro_trim_seconds, 0.0);
    a.analysis.as_mut().unwrap().beats.reverse();
    b.analysis.as_mut().unwrap().beats.reverse();
    let second = transition::select(&a, &b, &Options::default(), None, 0, 0).unwrap();
    assert_eq!(
        serde_json::to_value(first).unwrap(),
        serde_json::to_value(second).unwrap()
    );
}

#[test]
fn absent_or_weak_beats_fail_without_override() {
    let a = track(1, 10.0, &[]);
    let mut b = track(2, 10.0, &[0.5]);
    let error = transition::select(&a, &b, &Options::default(), None, 0, 0).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("Refusing an unaligned transition")
    );
    let manual = Override {
        outgoing_beat_seconds: 9.5,
        incoming_beat_seconds: 0.5,
        overlap_seconds: None,
    };
    let t = transition::select(&a, &b, &Options::default(), Some(&manual), 0, 0).unwrap();
    assert_eq!(t.source, "user_override");
    assert!(!t.warnings.is_empty());
    b.analysis.as_mut().unwrap().beats[0].score = 0.1;
    assert!(
        transition::select(&track(1, 10.0, &[9.5]), &b, &Options::default(), None, 0, 0).is_err()
    );
}

#[test]
fn bounds_bad_configuration_and_unknown_overrides_fail() {
    let a = track(1, 10.0, &[9.5]);
    let b = track(2, 10.0, &[0.5]);
    let manual = Override {
        outgoing_beat_seconds: 10.0,
        incoming_beat_seconds: 0.5,
        overlap_seconds: None,
    };
    assert!(transition::select(&a, &b, &Options::default(), Some(&manual), 0, 0).is_err());
    let options = Options {
        overlap: f64::NAN,
        ..Options::default()
    };
    assert!(options.validate().is_err());
    let mut overrides = Overrides::new();
    overrides.insert("1->99".into(), manual);
    assert!(transition::plan(&[a, b], &Options::default(), &overrides).is_err());
    assert!(transition::seconds_to_samples(-1.0).is_err());
    assert!(transition::seconds_to_samples(f64::INFINITY).is_err());
}

#[test]
fn adjacent_fades_must_not_collide() {
    let tracks = [
        track(1, 10.0, &[9.5]),
        track(2, 1.5, &[0.5, 1.0]),
        track(3, 10.0, &[0.5]),
    ];
    assert!(transition::plan(&tracks, &Options::default(), &Overrides::new()).is_err());
}

#[test]
fn odd_sample_overlap_still_aligns_both_beats_exactly() {
    let options = Options {
        overlap: 48001.0 / SAMPLE_RATE as f64,
        ..Options::default()
    };
    let a = track(1, 10.0, &[9.0]);
    let b = track(2, 10.0, &[1.0]);
    let t = transition::select(&a, &b, &options, None, 0, 0).unwrap();
    let fade_start = t.outgoing_end_sample - t.overlap_samples;
    assert_eq!(
        t.outgoing_beat_sample - fade_start,
        t.incoming_beat_sample - t.incoming_start_sample
    );
    assert_eq!(t.overlap_samples, 48001);
}

#[test]
fn bpm_difference_is_reported_not_corrected() {
    let a = track(1, 10.0, &[9.5]);
    let mut b = track(2, 10.0, &[0.5]);
    b.analysis.as_mut().unwrap().bpm = Some(130.0);
    let transitions = transition::plan(
        &[a.clone(), b.clone()],
        &Options::default(),
        &Overrides::new(),
    )
    .unwrap();
    assert!(transitions[0].warnings.iter().any(|w| w.contains("drift")));
    let graph = render::filter_graph(&[a, b], &transitions).unwrap();
    for forbidden in [
        "atempo",
        "asetrate",
        "rubberband",
        "scaletempo",
        "aresample",
        "minterpolate",
    ] {
        assert!(!graph.contains(forbidden));
    }
    assert!(graph.contains("c1=qsin:c2=qsin"));
    assert!(graph.contains("asetpts=PTS-STARTPTS"));
}

#[test]
fn missing_dependencies_and_models_have_actionable_errors() {
    let error = check_tool(&"/definitely/missing/ffmpeg".into(), "-version", "FFmpeg").unwrap_err();
    assert!(format!("{error:#}").contains("Install FFmpeg"));
    let dir = tempfile::tempdir().unwrap();
    assert!(
        format!("{:#}", models::verify(dir.path(), true).unwrap_err())
            .contains("playlist-mix models")
    );
}

#[test]
fn analysis_summary_is_finite_and_repeatable() {
    let t = track(1, 10.0, &[0.5, 1.0, 1.5, 2.0]);
    let (bpm, score, regularity) = analysis::summarize(&t.analysis.unwrap().beats);
    assert_eq!(bpm, Some(120.0));
    assert!((score - 0.9).abs() < 1e-10);
    assert_eq!(regularity, 1.0);
    assert_eq!(analysis::summarize(&[]), (None, 0.0, 0.0));
}

#[test]
fn manifest_roundtrip_preserves_order_and_decisions() {
    let dir = tempfile::tempdir().unwrap();
    let mut m = Manifest::new(
        "supplied-url".into(),
        "fixture".into(),
        vec![track(1, 10.0, &[9.5]), track(2, 10.0, &[0.5])],
    );
    m.transitions = transition::plan(&m.tracks, &Options::default(), &Overrides::new()).unwrap();
    let path = dir.path().join("manifest.json");
    m.save(&path).unwrap();
    let loaded = Manifest::load(&path).unwrap();
    assert_eq!(
        serde_json::to_value(m).unwrap(),
        serde_json::to_value(loaded).unwrap()
    );
    assert!(!path.with_extension("json.partial").exists());
}

#[test]
fn playlist_url_is_supplied_and_validated() {
    assert!(
        playlist::validate_url("https://music.youtube.com/playlist?list=test&other=literal")
            .is_ok()
    );
    for bad in [
        "",
        "https://example.com/playlist?list=x",
        "https://music.youtube.com/playlist",
        "--exec=bad",
    ] {
        assert!(playlist::validate_url(bad).is_err());
    }
}

fn ffmpeg_available() -> bool {
    if Command::new("ffmpeg")
        .arg("-version")
        .output()
        .is_ok_and(|o| o.status.success())
        && Command::new("ffprobe")
            .arg("-version")
            .output()
            .is_ok_and(|o| o.status.success())
    {
        true
    } else {
        eprintln!("SKIP: FFmpeg/FFprobe integration test; install FFmpeg to run it");
        false
    }
}

fn write_pcm(path: &Path, frequency: f64, phase: f64, beat_sample: usize) -> Vec<f32> {
    let frames = 4 * SAMPLE_RATE as usize;
    let mut samples = Vec::with_capacity(frames);
    let mut bytes = Vec::with_capacity(frames * 8);
    for frame in 0..frames {
        let tone = 0.1
            * (frame as f64 / SAMPLE_RATE as f64 * frequency * std::f64::consts::TAU + phase).sin();
        let value = (tone + if frame == beat_sample { 0.4 } else { 0.0 }) as f32;
        samples.push(value);
        for _ in 0..2 {
            bytes.extend(value.to_le_bytes());
        }
    }
    fs::write(path, bytes).unwrap();
    samples
}

#[test]
fn synthetic_render_aligns_impulses_and_has_equal_power_fades_without_speed_change() {
    if !ffmpeg_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let mut a = track(1, 4.0, &[3.0]);
    let mut b = track(2, 4.0, &[1.0]);
    a.pcm_path = Some(dir.path().join("first.f32"));
    b.pcm_path = Some(dir.path().join("second.f32"));
    let samples_a = write_pcm(
        a.pcm_path.as_ref().unwrap(),
        440.0,
        0.3,
        3 * SAMPLE_RATE as usize,
    );
    let samples_b = write_pcm(
        b.pcm_path.as_ref().unwrap(),
        880.0,
        0.7,
        SAMPLE_RATE as usize,
    );
    let tracks = [a, b];
    let transitions = transition::plan(&tracks, &Options::default(), &Overrides::new()).unwrap();
    let output = dir.path().join("mix.flac");
    render::render(
        &Tools::default(),
        &tracks,
        &transitions,
        &output,
        &dir.path().join("graph.txt"),
        Format::Flac,
        false,
    )
    .unwrap();
    let decoded = Command::new("ffmpeg")
        .args(["-v", "error", "-i"])
        .arg(&output)
        .args(["-f", "f32le", "-c:a", "pcm_f32le", "-"])
        .output()
        .unwrap();
    assert!(decoded.status.success());
    let samples: Vec<f32> = decoded
        .stdout
        .as_chunks::<8>()
        .0
        .iter()
        .map(|c| f32::from_le_bytes(c[..4].try_into().unwrap()))
        .collect();
    assert_eq!(
        samples.len() as u64,
        render::expected_frames(&tracks, &transitions).unwrap()
    );
    assert_eq!(samples.len(), 6 * SAMPLE_RATE as usize);
    let headroom = 1.0 / 2f64.sqrt();
    // Waveform/sample identity outside the fade rules out pitch and speed changes.
    for i in 1000..2000 {
        assert!((samples[i] as f64 - samples_a[i] as f64 * headroom).abs() < 2e-6);
    }
    for i in 240000..241000 {
        assert!((samples[i] as f64 - samples_b[i - 96000] as f64 * headroom).abs() < 2e-6);
    }
    let center = 3 * SAMPLE_RATE as usize;
    assert!(
        (samples[center] as f64 - (samples_a[center] as f64 + samples_b[48000] as f64) / 2.0).abs()
            < 5e-5
    );
    assert!(samples[center] > 0.4); // Both beat impulses occur at the same output sample.
    assert!(samples[center - 1].abs() < 0.11 && samples[center + 1].abs() < 0.11);
    let probe = 132012; // Interior of the overlap, away from the aligned impulse.
    let fraction = (probe as f64 - 120000.0) / 48000.0;
    let expected = headroom
        * (samples_a[probe] as f64 * (fraction * std::f64::consts::FRAC_PI_2).cos()
            + samples_b[probe - 96000] as f64 * (fraction * std::f64::consts::FRAC_PI_2).sin());
    assert!((samples[probe] as f64 - expected).abs() < 5e-5);
    assert!(samples.iter().all(|v| v.abs() < 1.0));
    assert!(
        render::render(
            &Tools::default(),
            &tracks,
            &transitions,
            &output,
            &dir.path().join("graph.txt"),
            Format::Flac,
            false
        )
        .is_err()
    );
    let mp3 = dir.path().join("mix.mp3");
    render::render(
        &Tools::default(),
        &tracks,
        &transitions,
        &mp3,
        &dir.path().join("graph.txt"),
        Format::Mp3,
        false,
    )
    .unwrap();
    assert!(fs::metadata(mp3).unwrap().len() > 0);
}

#[test]
fn unchanged_audio_reuses_pcm_but_changed_source_or_corrupt_cache_invalidates_analysis() {
    if !ffmpeg_available() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("source.wav");
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: SAMPLE_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let write_wav = |value: i16| {
        let mut w = hound::WavWriter::create(&path, spec).unwrap();
        for _ in 0..48000 {
            w.write_sample(value).unwrap();
        }
        w.finalize().unwrap();
    };
    write_wav(100);
    let mut t = track(1, 1.0, &[]);
    t.path = path.clone();
    let tools = Tools::default();
    let version = tools.check(true).unwrap()["ffmpeg"].clone();
    assert!(analysis::prepare_pcm(&mut t, dir.path(), &tools, &version).unwrap());
    t.analysis = track(1, 1.0, &[0.5]).analysis;
    let old = t.sha256.clone();
    assert!(!analysis::prepare_pcm(&mut t, dir.path(), &tools, &version).unwrap());
    assert!(t.analysis.is_some());
    write_wav(101);
    assert!(analysis::prepare_pcm(&mut t, dir.path(), &tools, &version).unwrap());
    assert_ne!(t.sha256, old);
    assert!(t.analysis.is_none());
    let pcm = t.pcm_path.clone().unwrap();
    let mut bytes = fs::read(&pcm).unwrap();
    bytes[0] ^= 1;
    fs::write(&pcm, bytes).unwrap();
    assert!(analysis::prepare_pcm(&mut t, dir.path(), &tools, &version).unwrap());
    assert_eq!(t.pcm_sha256.as_ref().unwrap(), &hash_file(&pcm).unwrap());
}

#[cfg(unix)]
#[test]
fn download_failure_is_fatal_and_url_is_a_literal_argument_not_shell_code() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("fake-yt-dlp");
    let args_log = dir.path().join("args.txt");
    fs::write(
        &script,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\necho 'access denied' >&2\nexit 42\n",
            args_log.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
    let tools = Tools {
        yt_dlp: script.into_os_string(),
        ..Tools::default()
    };
    let marker = dir.path().join("SHOULD_NOT_EXIST");
    let mut t = track(1, 10.0, &[]);
    t.url = format!(
        "https://www.youtube.com/watch?v=literal&x=$(touch {})",
        marker.display()
    );
    t.path = dir.path().join("00001-id1.mp3");
    let error = playlist::download(&tools, &t, &dir.path().join("staging"), None).unwrap_err();
    assert!(format!("{error:#}").contains("access denied"));
    let log = fs::read_to_string(args_log).unwrap();
    assert!(log.lines().any(|line| line == t.url));
    assert!(log.contains("--no-skip-unavailable-fragments"));
    assert!(!marker.exists());
    assert!(!t.path.exists());
}

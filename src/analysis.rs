use crate::{
    ANALYSIS_RATE, SAMPLE_RATE,
    manifest::{Analysis, Beat, Track, hash_bytes, hash_file},
    models,
    process::{Tools, capture},
};
use anyhow::{Context, Result, ensure};
use beat_this::{BeatThis, RtenRuntime, Runtime};
use std::{
    fs::{self, File},
    io::Read,
    path::Path,
    process::Command,
};

pub type Detector = BeatThis<<RtenRuntime as Runtime>::Model>;

pub fn load_detector(dir: &Path, small: bool) -> Result<Detector> {
    models::verify(dir, small)?;
    let (mel, beat) = models::paths(dir, small);
    BeatThis::new(&RtenRuntime, &mel, &beat).context("Cannot load pinned beat models")
}

pub fn prepare_pcm(
    track: &mut Track,
    cache: &Path,
    tools: &Tools,
    ffmpeg_version: &str,
) -> Result<bool> {
    let source_hash = hash_file(&track.path)?;
    let key = hash_bytes(
        format!("pcm-v2-strict;{source_hash};{ffmpeg_version};48000;stereo;f32le").as_bytes(),
    );
    let path = cache.join(format!("{:05}-{key}.f32", track.index));
    let cached = track.sha256.as_deref() == Some(&source_hash)
        && track.pcm_key.as_deref() == Some(&key)
        && track.pcm_path.as_ref() == Some(&path)
        && path.is_file()
        && track.frames.is_some_and(|frames| {
            frames > 0 && path.metadata().is_ok_and(|m| m.len() == frames * 8)
        })
        && track.pcm_sha256.as_deref() == Some(&hash_file(&path)?);
    if cached {
        return Ok(false);
    }
    fs::create_dir_all(cache)?;
    let partial = path.with_extension("f32.partial");
    let result = (|| -> Result<()> {
        let mut cmd = Command::new(&tools.ffmpeg);
        cmd.args([
            "-nostdin",
            "-hide_banner",
            "-loglevel",
            "error",
            "-xerror",
            "-y",
            "-threads",
            "1",
            "-i",
        ])
        .arg(&track.path)
        .args([
            "-map",
            "0:a:0",
            "-vn",
            "-ar",
            "48000",
            "-ac",
            "2",
            "-c:a",
            "pcm_f32le",
            "-f",
            "f32le",
        ])
        .arg(&partial);
        capture(&mut cmd, &format!("PCM decode of index {}", track.index))?;
        let len = partial.metadata()?.len();
        ensure!(
            len > 0 && len % 8 == 0,
            "Index {} has invalid/empty decoded PCM",
            track.index
        );
        fs::rename(&partial, &path)?;
        track.sha256 = Some(source_hash);
        track.pcm_key = Some(key);
        track.pcm_sha256 = Some(hash_file(&path)?);
        track.pcm_path = Some(path.clone());
        track.frames = Some(len / 8);
        track.duration = Some((len / 8) as f64 / SAMPLE_RATE as f64);
        track.analysis = None;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(partial);
    }
    result?;
    Ok(true)
}

pub fn cache_key(track: &Track, detector_stamp: &str) -> Result<String> {
    Ok(hash_bytes(
        format!(
            "{};{};lock={}",
            track.pcm_sha256.as_deref().context("PCM missing")?,
            detector_stamp,
            hash_bytes(include_bytes!("../Cargo.lock"))
        )
        .as_bytes(),
    ))
}

fn median(values: &mut [f64]) -> f64 {
    values.sort_by(f64::total_cmp);
    let n = values.len();
    if n.is_multiple_of(2) {
        (values[n / 2 - 1] + values[n / 2]) / 2.0
    } else {
        values[n / 2]
    }
}

pub fn summarize(beats: &[Beat]) -> (Option<f64>, f64, f64) {
    let confidence = if beats.is_empty() {
        0.0
    } else {
        beats.iter().map(|b| b.score).sum::<f64>() / beats.len() as f64
    };
    let mut intervals: Vec<f64> = beats
        .windows(2)
        .map(|w| w[1].seconds - w[0].seconds)
        .filter(|v| *v > 0.0)
        .collect();
    if intervals.is_empty() {
        return (None, confidence, 0.0);
    }
    let step = median(&mut intervals);
    let mut deviations: Vec<f64> = intervals.iter().map(|v| (v - step).abs()).collect();
    let regularity = (1.0 - median(&mut deviations) / step).clamp(0.0, 1.0);
    (Some(60.0 / step), confidence, regularity)
}

pub fn analyze(
    track: &mut Track,
    detector: &mut Detector,
    tools: &Tools,
    stamp: &str,
) -> Result<()> {
    let pcm = track
        .pcm_path
        .as_ref()
        .context("Track has not been decoded")?;
    let mono_path = pcm.with_extension("mono.partial");
    let result = (|| -> Result<Analysis> {
        let mut cmd = Command::new(&tools.ffmpeg);
        cmd.args([
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
        .arg(pcm)
        .args([
            "-ar",
            "22050",
            "-ac",
            "1",
            "-c:a",
            "pcm_f32le",
            "-f",
            "f32le",
        ])
        .arg(&mono_path);
        capture(&mut cmd, "mono analysis decode")?;
        let mut stream = detector.stream(ANALYSIS_RATE)?;
        let mut file = File::open(&mono_path)?;
        let mut buffer = [0u8; 65536];
        // read_exact per chunk (or a final sized chunk) avoids misaligned f32 reads.
        let mut remaining = file.metadata()?.len();
        ensure!(remaining > 0 && remaining % 4 == 0, "Invalid analysis PCM");
        while remaining > 0 {
            let n = remaining.min(buffer.len() as u64) as usize;
            file.read_exact(&mut buffer[..n])?;
            let samples: Vec<f32> = buffer[..n]
                .as_chunks::<4>()
                .0
                .iter()
                .map(|c| f32::from_le_bytes(*c))
                .collect();
            ensure!(
                samples.iter().all(|v| v.is_finite()),
                "Non-finite audio samples at index {}",
                track.index
            );
            stream.push(&samples)?;
            remaining -= n as u64;
        }
        let raw = stream.finish()?;
        let frames = track.frames.context("Missing PCM frame count")?;
        let mut beats = Vec::new();
        for seconds in raw.beats {
            let seconds = f64::from(seconds);
            if !seconds.is_finite() || seconds < 0.0 {
                continue;
            }
            let sample = (seconds * SAMPLE_RATE as f64).round() as u64;
            if sample >= frames {
                continue;
            }
            let logit = raw
                .beat_logits
                .get((seconds * 50.0).round() as usize)
                .copied()
                .unwrap_or(-1000.0);
            ensure!(
                logit.is_finite(),
                "Beat model returned non-finite confidence"
            );
            beats.push(Beat {
                seconds: sample as f64 / SAMPLE_RATE as f64,
                sample,
                score: 1.0 / (1.0 + (-f64::from(logit)).exp()),
            });
        }
        beats.sort_by(|a, b| {
            a.sample
                .cmp(&b.sample)
                .then_with(|| b.score.total_cmp(&a.score))
        });
        beats.dedup_by_key(|b| b.sample);
        let (bpm, confidence, regularity) = summarize(&beats);
        let mut warnings = vec![];
        if beats.len() < 3 {
            warnings.push("Too few detected beats for reliable BPM/transition estimation".into());
        }
        if confidence < 0.6 {
            warnings.push("Low mean model activation; timestamps may be unreliable".into());
        }
        if regularity < 0.8 {
            warnings
                .push("Irregular beat intervals: BPM may vary, or detection may be wrong".into());
        }
        Ok(Analysis {
            cache_key: cache_key(track, stamp)?,
            detector: stamp.into(),
            bpm,
            confidence,
            confidence_kind: "mean sigmoid beat-logit at selected peaks; uncalibrated".into(),
            interval_regularity: regularity,
            beats,
            downbeats: raw.downbeats.iter().map(|x| f64::from(*x)).collect(),
            warnings,
        })
    })();
    let _ = fs::remove_file(mono_path);
    track.analysis = Some(
        result
            .with_context(|| format!("Beat analysis failed at playlist index {}", track.index))?,
    );
    Ok(())
}

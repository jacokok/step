//! Generate two deterministic, music-like WAV fixtures and an explicit transition override.
//! No network, Python, or copyrighted audio is needed.
use anyhow::Result;
use std::{fs, path::PathBuf};

fn main() -> Result<()> {
    let dir = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or("examples/generated/tracks".into());
    fs::create_dir_all(&dir)?;
    let sr = 48000;
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate: sr,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    for (index, phase) in [(1, 0.0), (2, 0.2)] {
        let path = dir.join(format!("{index:05}-synthetic.wav"));
        let mut wav = hound::WavWriter::create(&path, spec)?;
        let mut rng: u32 = 42;
        for frame in 0..16 * sr {
            let t = frame as f64 / sr as f64;
            let beat_age = (t - 0.5).rem_euclid(0.5);
            let hat_age = (t - 0.5).rem_euclid(0.25);
            let snare_age = (t - 1.0).rem_euclid(1.0);
            rng = rng.wrapping_mul(1664525).wrapping_add(1013904223);
            let noise = rng as f64 / u32::MAX as f64 * 2.0 - 1.0;
            let kick = 0.55
                * (-beat_age * 28.0).exp()
                * (std::f64::consts::TAU
                    * (55.0 * beat_age + 90.0 * (1.0 - (-beat_age * 20.0).exp()) / 20.0))
                    .sin();
            let hat = 0.07 * noise * (-hat_age * 90.0).exp();
            let snare = 0.14 * noise * (-snare_age * 35.0).exp();
            let bass =
                0.08 * (-beat_age * 4.0).exp() * (std::f64::consts::TAU * 110.0 * t + phase).sin();
            let value =
                ((kick + hat + snare + bass).clamp(-1.0, 1.0) * i16::MAX as f64).round() as i16;
            wav.write_sample(value)?;
            wav.write_sample(value)?;
        }
        wav.finalize()?;
        println!("{}", path.display());
    }
    fs::write(
        dir.join("overrides.json"),
        "{\n  \"1->2\": {\n    \"outgoing_beat_seconds\": 15.5,\n    \"incoming_beat_seconds\": 0.5,\n    \"overlap_seconds\": 1.0\n  }\n}\n",
    )?;
    Ok(())
}

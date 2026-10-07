use crate::{
    SAMPLE_RATE,
    manifest::{Track, Transition},
    process::{Tools, capture},
};
use anyhow::{Context, Result, ensure};
use clap::ValueEnum;
use serde::Serialize;
use std::{fs, path::Path, process::Command};

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    Flac,
    Mp3,
}
impl Format {
    pub fn extension(self) -> &'static str {
        match self {
            Self::Flac => "flac",
            Self::Mp3 => "mp3",
        }
    }
    pub fn infer(output: &Path, explicit: Option<Self>) -> Result<Self> {
        let extension = output
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let format = explicit.unwrap_or(Self::Mp3);
        ensure!(
            extension == format.extension(),
            "Output extension must be .{}; MP3 is the default. Use --format flac explicitly for a .flac output",
            format.extension()
        );
        Ok(format)
    }
}

pub fn trim_bounds(tracks: &[Track], transitions: &[Transition]) -> Result<Vec<(u64, u64)>> {
    ensure!(
        !tracks.is_empty() && transitions.len() + 1 == tracks.len(),
        "Invalid track/transition count"
    );
    let mut bounds = vec![];
    for (i, track) in tracks.iter().enumerate() {
        let start = if i == 0 {
            0
        } else {
            transitions[i - 1].incoming_start_sample
        };
        let end = if i == transitions.len() {
            track.frames.context("Missing PCM length")?
        } else {
            transitions[i].outgoing_end_sample
        };
        ensure!(
            end > start && end <= track.frames.context("Missing PCM length")?,
            "Invalid retained segment at index {}",
            track.index
        );
        let fade_in = if i == 0 {
            0
        } else {
            transitions[i - 1].overlap_samples
        };
        let fade_out = if i == transitions.len() {
            0
        } else {
            transitions[i].overlap_samples
        };
        ensure!(
            end - start >= fade_in + fade_out,
            "Index {} is too short for non-overlapping crossfades",
            track.index
        );
        if i < transitions.len() {
            ensure!(
                transitions[i].outgoing_index == track.index
                    && transitions[i].incoming_index == tracks[i + 1].index,
                "Transitions are not in playlist order"
            );
        }
        bounds.push((start, end));
    }
    Ok(bounds)
}

pub fn expected_frames(tracks: &[Track], transitions: &[Transition]) -> Result<u64> {
    let kept: u64 = trim_bounds(tracks, transitions)?
        .iter()
        .map(|(start, end)| end - start)
        .sum();
    Ok(kept - transitions.iter().map(|t| t.overlap_samples).sum::<u64>())
}

/// Input PCM already has a common sample rate. No rate manipulation, stretching,
/// or per-track encoding appears in this graph: only trims, fades and headroom.
pub fn filter_graph(tracks: &[Track], transitions: &[Transition]) -> Result<String> {
    let bounds = trim_bounds(tracks, transitions)?;
    let mut graph = Vec::new();
    for (i, (start, end)) in bounds.iter().enumerate() {
        graph.push(format!(
            "[{i}:a]atrim=start_sample={start}:end_sample={end},asetpts=PTS-STARTPTS[t{i}]"
        ));
    }
    let mut previous = "t0".to_string();
    for (i, transition) in transitions.iter().enumerate() {
        let label = format!("mix{i}");
        graph.push(format!(
            "[{previous}][t{}]acrossfade=ns={}:o=1:c1=qsin:c2=qsin[{label}]",
            i + 1,
            transition.overlap_samples
        ));
        previous = label;
    }
    // Equal-power fades can add +3 dB for perfectly correlated sources.
    graph.push(format!("[{previous}]volume=0.7071067811865475[out]"));
    Ok(graph.join(";\n"))
}

pub fn render(
    tools: &Tools,
    tracks: &[Track],
    transitions: &[Transition],
    output: &Path,
    graph_path: &Path,
    format: Format,
    force: bool,
) -> Result<()> {
    ensure!(
        force || !output.exists(),
        "Output {} already exists; choose a new path or pass --force",
        output.display()
    );
    fs::write(graph_path, filter_graph(tracks, transitions)?)?;
    let parent = output.parent().context("Output has no parent directory")?;
    fs::create_dir_all(parent)?;
    let name = output
        .file_name()
        .context("Output has no filename")?
        .to_string_lossy();
    let partial = parent.join(format!(".{name}.partial.{}", format.extension()));
    let result = (|| -> Result<()> {
        let mut command = Command::new(&tools.ffmpeg);
        command.args([
            "-nostdin",
            "-hide_banner",
            "-loglevel",
            "error",
            "-xerror",
            "-y",
            "-filter_complex_threads",
            "1",
        ]);
        for track in tracks {
            command
                .args(["-f", "f32le", "-ar", "48000", "-ac", "2", "-i"])
                .arg(track.pcm_path.as_ref().context("PCM path missing")?);
        }
        command
            .arg("-filter_complex")
            .arg(fs::read_to_string(graph_path)?)
            .args([
                "-map",
                "[out]",
                "-map_metadata",
                "-1",
                "-ar",
                "48000",
                "-threads",
                "1",
            ]);
        match format {
            Format::Flac => {
                command.args([
                    "-c:a",
                    "flac",
                    "-sample_fmt",
                    "s32",
                    "-bits_per_raw_sample",
                    "24",
                    "-compression_level",
                    "8",
                    "-f",
                    "flac",
                ]);
            }
            Format::Mp3 => {
                command.args([
                    "-c:a",
                    "libmp3lame",
                    "-q:a",
                    "0",
                    "-write_xing",
                    "1",
                    "-f",
                    "mp3",
                ]);
            }
        }
        command.arg(&partial);
        capture(&mut command, "final FFmpeg mix/encode")?;
        ensure!(
            partial.metadata()?.len() > 0,
            "FFmpeg produced an empty mix"
        );
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
            .arg(&partial);
        let codec = capture(&mut probe, "final output validation")?;
        ensure!(
            String::from_utf8_lossy(&codec).trim() == format.extension(),
            "Final output has the wrong audio codec"
        );
        fs::rename(&partial, output).context(
            "Cannot commit final output; any existing output was left untouched if encoding failed",
        )?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(partial);
    }
    result?;
    println!(
        "Created {} ({:.3}s before codec padding)",
        output.display(),
        expected_frames(tracks, transitions)? as f64 / SAMPLE_RATE as f64
    );
    Ok(())
}

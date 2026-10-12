//! Loopback-only browser editor. No remote assets or beat-model downloads.
use crate::{
    SAMPLE_RATE, analysis, cut,
    manifest::{Analysis, Manifest, Track, hash_bytes},
    playlist,
    process::{Tools, capture},
};
use anyhow::{Context, Result, ensure};
use clap::Args;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    fs::{self, File, OpenOptions},
    io::{BufRead, BufReader, Read, Seek, SeekFrom, Write},
    net::{TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::Command,
    time::Duration,
};

#[derive(Debug, Args)]
pub struct EditArgs {
    /// State, PCM cache and original backups.
    #[arg(long, default_value = "out/mix.work")]
    pub work_dir: PathBuf,
    /// Index-prefixed MP3/WAV/FLAC files; defaults to <work-dir>/tracks.
    #[arg(long)]
    pub local_tracks: Option<PathBuf>,
    /// Loopback port; zero chooses an available port.
    #[arg(long, default_value_t = 0)]
    pub port: u16,
}

struct Session {
    work: PathBuf,
    manifest: Manifest,
    tools: Tools,
}
impl Session {
    fn prepare(&mut self, index: u32) -> Result<usize> {
        let pos = self
            .manifest
            .tracks
            .iter()
            .position(|t| t.index == index)
            .context("Unknown track")?;
        let changed = analysis::prepare_pcm(
            &mut self.manifest.tracks[pos],
            &self.work.join("pcm"),
            &self.tools,
            &self.manifest.tools["ffmpeg"],
        )?;
        if changed {
            self.manifest.transitions.clear();
            self.manifest.output_plan_key = None;
        }
        self.manifest.save(&self.work.join("manifest.json"))?;
        Ok(pos)
    }
    fn detail(&mut self, index: u32) -> Result<Value> {
        let pos = self.prepare(index)?;
        let track = &self.manifest.tracks[pos];
        let frames = track.frames.context("PCM missing")?;
        let peaks = waveform(
            track.pcm_path.as_deref().context("PCM missing")?,
            frames,
            2400,
        )?;
        // PCM16 WAV is playable by browsers regardless of the source format.
        let preview = preview_path(track)?;
        if !preview.exists() {
            let partial = preview.with_extension("wav.partial");
            let result = capture(
                Command::new(&self.tools.ffmpeg)
                    .args([
                        "-nostdin", "-v", "error", "-xerror", "-y", "-f", "f32le", "-ar", "48000",
                        "-ac", "2", "-i",
                    ])
                    .arg(track.pcm_path.as_ref().unwrap())
                    .args(["-c:a", "pcm_s16le", "-f", "wav"])
                    .arg(&partial),
                "editor playback decode",
            );
            if let Err(error) = result {
                let _ = fs::remove_file(&partial);
                return Err(error);
            }
            fs::rename(partial, preview)?;
        }
        let beats: Vec<f64> = track
            .analysis
            .as_ref()
            .map(|a| {
                a.beats
                    .iter()
                    .map(|b| b.sample as f64 / SAMPLE_RATE as f64)
                    .collect()
            })
            .unwrap_or_default();
        Ok(
            json!({"index": index, "title": track.title, "duration": track.duration, "hash": track.sha256, "peaks": peaks, "beats": beats}),
        )
    }
    fn remove(&mut self, request: Removal) -> Result<Value> {
        let pos = self.prepare(request.index)?;
        let track = self.manifest.tracks[pos].clone();
        ensure!(
            track.sha256.as_deref() == Some(&request.hash),
            "Track changed; reload before cutting"
        );
        let frames = track.frames.context("PCM missing")?;
        let ranges = retained_ranges(frames, request.start, request.end)?;
        ensure!(
            self.manifest
                .config
                .entry("cut_history".into())
                .or_insert_with(|| json!([]))
                .is_array(),
            "Invalid cut history"
        );
        let backup = cut::backup_original(&track, &request.hash, &self.work.join("originals"))?;
        cut::replace_ranges(&self.tools, &track, &ranges)?;
        self.manifest.config.get_mut("cut_history").unwrap().as_array_mut().unwrap().push(json!({"index": track.index, "backup": backup, "original_sha256": request.hash, "removed_start_sample": (request.start * SAMPLE_RATE as f64).round() as u64, "removed_end_sample": (request.end * SAMPLE_RATE as f64).round() as u64, "original_frames": frames, "kind": "manual"}));
        let previous_analysis = track.analysis;
        let mut changed = Track::new(track.index, track.id, track.title, track.url, track.path);
        changed.downloaded = true;
        self.manifest.tracks[pos] = changed;
        self.manifest.transitions.clear();
        self.manifest.output_plan_key = None;
        self.manifest.last_error = None;
        // Persist invalidation before any fallible decode of the replaced file.
        self.manifest.save(&self.work.join("manifest.json"))?;
        self.prepare(request.index)?;
        if let Some(previous) = previous_analysis {
            let changed = &mut self.manifest.tracks[pos];
            let key = format!(
                "edited:{}",
                analysis::cache_key(changed, &previous.detector)?
            );
            changed.analysis = Some(remap_analysis(
                previous,
                &ranges,
                changed.frames.context("PCM missing")?,
                key,
            ));
            self.manifest.save(&self.work.join("manifest.json"))?;
        }
        Ok(
            json!({"backup": backup, "message": "Cut saved. Remaining beat markers retained for editing; run step beat before mixing."}),
        )
    }
}

#[derive(Deserialize)]
struct Removal {
    index: u32,
    hash: String,
    start: f64,
    end: f64,
}

fn retained_ranges(frames: u64, start: f64, end: f64) -> Result<Vec<(u64, u64)>> {
    let duration = frames as f64 / SAMPLE_RATE as f64;
    ensure!(
        start.is_finite() && end.is_finite() && start >= 0.0 && start < end && end <= duration,
        "Select a valid section to remove"
    );
    let start = (start * SAMPLE_RATE as f64).round() as u64;
    let end = (end * SAMPLE_RATE as f64).round() as u64;
    ensure!(
        start < end && (start > 0 || end < frames),
        "Cannot remove an empty selection or the entire track"
    );
    let mut ranges = vec![];
    if start > 0 {
        ranges.push((0, start));
    }
    if end < frames {
        ranges.push((end, frames));
    }
    Ok(ranges)
}

/// Keep markers in retained, half-open sample ranges and move them onto the
/// edited timeline. The edited cache key deliberately forces re-analysis for
/// mixing; these markers are useful for snapping, not a fresh detector result.
fn remap_analysis(
    mut previous: Analysis,
    ranges: &[(u64, u64)],
    frames: u64,
    key: String,
) -> Analysis {
    let map_sample = |sample: u64| {
        let mut offset = 0;
        for &(start, end) in ranges {
            if (start..end).contains(&sample) {
                let mapped = offset + sample - start;
                return (mapped < frames).then_some(mapped);
            }
            offset += end - start;
        }
        None
    };
    previous.beats = previous
        .beats
        .into_iter()
        .filter_map(|mut beat| {
            let sample = map_sample(beat.sample)?;
            beat.sample = sample;
            beat.seconds = sample as f64 / SAMPLE_RATE as f64;
            Some(beat)
        })
        .collect();
    previous.downbeats = previous
        .downbeats
        .into_iter()
        .filter(|s| s.is_finite() && *s >= 0.0)
        .filter_map(|s| map_sample((s * SAMPLE_RATE as f64).round() as u64))
        .map(|s| s as f64 / SAMPLE_RATE as f64)
        .collect();
    let (bpm, confidence, regularity) = analysis::summarize(&previous.beats);
    previous.bpm = bpm;
    previous.confidence = confidence;
    previous.interval_regularity = regularity;
    previous.cache_key = key;
    let warning = "Beat markers shifted after manual editing; re-analyze before mixing";
    if !previous.warnings.iter().any(|w| w == warning) {
        previous.warnings.push(warning.into());
    }
    previous
}

fn preview_path(track: &Track) -> Result<PathBuf> {
    Ok(track
        .pcm_path
        .as_ref()
        .context("PCM missing")?
        .with_extension("preview.wav"))
}

fn waveform(path: &Path, frames: u64, bins: usize) -> Result<Vec<f32>> {
    let mut peaks = vec![0.0f32; bins.min(frames as usize)];
    let mut reader = BufReader::new(File::open(path)?);
    let mut bytes = [0; 8];
    for i in 0..frames {
        reader.read_exact(&mut bytes)?;
        let l = f32::from_le_bytes(bytes[..4].try_into()?);
        let r = f32::from_le_bytes(bytes[4..].try_into()?);
        ensure!(l.is_finite() && r.is_finite(), "Non-finite audio samples");
        let bin = (i * peaks.len() as u64 / frames) as usize;
        peaks[bin] = peaks[bin].max(l.abs().max(r.abs()));
    }
    Ok(peaks)
}

pub fn run(args: EditArgs, tools: &Tools) -> Result<()> {
    let versions = tools.check(true)?;
    fs::create_dir_all(&args.work_dir)?;
    let work = fs::canonicalize(args.work_dir)?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(work.join("workflow.lock"))?;
    lock.try_lock()
        .context("Work directory is locked by another run; close it before editing")?;
    let local = fs::canonicalize(args.local_tracks.unwrap_or_else(|| work.join("tracks")))
        .context("Cannot open tracks directory; run step get or supply --local-tracks")?;
    let path = work.join("manifest.json");
    let mut manifest = if path.exists() {
        Manifest::load(&path)?
    } else {
        Manifest::new(String::new(), format!("local:{}", local.display()), vec![])
    };
    manifest.tracks = playlist::import_local(&local)?
        .into_iter()
        .map(|t| {
            manifest
                .tracks
                .iter()
                .find(|old| old.index == t.index && old.path == t.path)
                .cloned()
                .unwrap_or(t)
        })
        .collect();
    ensure!(!manifest.tracks.is_empty(), "No tracks available");
    manifest.tools.extend(versions);
    manifest.save(&path)?;
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, args.port))?;
    let host = listener.local_addr()?.to_string();
    // Per-session mutation token, also protected by strict Host/Origin checks.
    let token = hash_bytes(
        format!(
            "{host}-{:?}-{}",
            std::time::SystemTime::now(),
            std::process::id()
        )
        .as_bytes(),
    );
    println!(
        "Open http://{host} in your browser. Ctrl+C stops the editor.\nOriginals are backed up; close the editor and run step beat after cuts."
    );
    let mut session = Session {
        work,
        manifest,
        tools: tools.clone(),
    };
    for stream in listener.incoming() {
        let mut stream = stream?;
        stream.set_read_timeout(Some(Duration::from_secs(10)))?;
        stream.set_write_timeout(Some(Duration::from_secs(30)))?;
        if let Err(error) = serve(&mut stream, &mut session, &host, &token) {
            let _ = response(
                &mut stream,
                "400 Bad Request",
                "application/json",
                serde_json::to_string(&json!({"error": format!("{error:#}")}))?.as_bytes(),
            );
        }
    }
    Ok(())
}

fn response(stream: &mut TcpStream, status: &str, kind: &str, body: &[u8]) -> Result<()> {
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n\r\n",
        body.len()
    )?;
    stream.write_all(body)?;
    Ok(())
}

fn serve(stream: &mut TcpStream, session: &mut Session, host: &str, token: &str) -> Result<()> {
    let mut reader = BufReader::new(&mut *stream);
    let mut headers = String::new();
    loop {
        let mut line = String::new();
        ensure!(
            reader.by_ref().take(8193).read_line(&mut line)? > 0,
            "Incomplete request"
        );
        headers.push_str(&line);
        ensure!(headers.len() <= 8192, "Headers too large");
        if line == "\r\n" {
            break;
        }
    }
    let mut lines = headers.lines();
    let request: Vec<_> = lines
        .next()
        .context("Missing request")?
        .split_whitespace()
        .collect();
    ensure!(request.len() == 3, "Invalid request");
    let fields: Vec<_> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.to_ascii_lowercase(), v.trim()))
        .collect();
    let field = |key: &str| fields.iter().find(|(k, _)| k == key).map(|(_, v)| *v);
    let length: usize = field("content-length").unwrap_or("0").parse()?;
    ensure!(length <= 4096, "Body too large");
    let mut body = vec![0; length];
    // Drain bounded bodies before rejecting a request so clients receive the
    // error response instead of a TCP reset caused by unread bytes.
    reader.read_exact(&mut body)?;
    ensure!(field("host") == Some(host), "Invalid Host");
    if let Some(origin) = field("origin") {
        ensure!(origin == format!("http://{host}"), "Invalid Origin");
    }
    ensure!(
        field("transfer-encoding").is_none(),
        "Unsupported transfer encoding"
    );
    let (method, path) = (request[0], request[1].split('?').next().unwrap());
    if method == "POST" && path == "/api/remove" {
        ensure!(field("x-step-token") == Some(token), "Invalid editor token");
        ensure!(field("content-length").is_some(), "Missing body length");
        let result = session.remove(serde_json::from_slice(&body)?)?;
        return response(
            stream,
            "200 OK",
            "application/json",
            &serde_json::to_vec(&result)?,
        );
    }
    ensure!(method == "GET", "Unsupported method");
    if path == "/" {
        return response(
            stream,
            "200 OK",
            "text/html; charset=utf-8",
            include_str!("editor.html")
                .replace("__TOKEN__", token)
                .as_bytes(),
        );
    }
    if path == "/api/tracks" {
        let tracks: Vec<_> = session
            .manifest
            .tracks
            .iter()
            .map(|t| json!({"index": t.index, "title": t.title}))
            .collect();
        return response(
            stream,
            "200 OK",
            "application/json",
            &serde_json::to_vec(&tracks)?,
        );
    }
    if let Some(index) = path.strip_prefix("/api/track/") {
        let detail = session.detail(index.parse()?)?;
        return response(
            stream,
            "200 OK",
            "application/json",
            &serde_json::to_vec(&detail)?,
        );
    }
    if let Some(index) = path.strip_prefix("/audio/") {
        let index: u32 = index.parse()?;
        let track = session
            .manifest
            .tracks
            .iter()
            .find(|t| t.index == index)
            .context("Unknown track")?;
        let mut file = File::open(preview_path(track)?)?;
        let len = file.metadata()?.len();
        let range = field("range").map(|v| parse_range(v, len)).transpose()?;
        let (start, end) = range.unwrap_or((0, len - 1));
        file.seek(SeekFrom::Start(start))?;
        write!(
            stream,
            "HTTP/1.1 {}\r\nContent-Type: audio/wav\r\nAccept-Ranges: bytes\r\nCache-Control: no-store\r\nContent-Length: {}\r\nConnection: close\r\n",
            if range.is_some() {
                "206 Partial Content"
            } else {
                "200 OK"
            },
            end - start + 1
        )?;
        if range.is_some() {
            write!(stream, "Content-Range: bytes {start}-{end}/{len}\r\n")?;
        }
        write!(stream, "\r\n")?;
        std::io::copy(&mut file.take(end - start + 1), stream)?;
        return Ok(());
    }
    response(stream, "404 Not Found", "text/plain", b"Not found")
}

fn parse_range(value: &str, len: u64) -> Result<(u64, u64)> {
    let (start, end) = value
        .strip_prefix("bytes=")
        .context("Invalid range")?
        .split_once('-')
        .context("Invalid range")?;
    ensure!(len > 0 && !end.contains(','), "Invalid range");
    let (start, end) = if start.is_empty() {
        let suffix: u64 = end.parse()?;
        ensure!(suffix > 0, "Invalid range");
        (len.saturating_sub(suffix), len - 1)
    } else {
        (
            start.parse()?,
            if end.is_empty() {
                len - 1
            } else {
                end.parse::<u64>()?.min(len - 1)
            },
        )
    };
    ensure!(start <= end && end < len, "Invalid range");
    Ok((start, end))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn removal_bounds() {
        assert_eq!(
            retained_ranges(48000, 0., 0.25).unwrap(),
            vec![(12000, 48000)]
        );
        assert_eq!(retained_ranges(48000, 0.75, 1.).unwrap(), vec![(0, 36000)]);
        assert_eq!(
            retained_ranges(48000, 0.25, 0.75).unwrap(),
            vec![(0, 12000), (36000, 48000)]
        );
        for (a, b) in [(0., 1.), (-1., 0.5), (0.5, 0.5), (0., 2.), (f64::NAN, 0.5)] {
            assert!(retained_ranges(48000, a, b).is_err());
        }
    }
    #[test]
    fn byte_ranges() {
        assert_eq!(parse_range("bytes=0-", 100).unwrap(), (0, 99));
        assert_eq!(parse_range("bytes=10-20", 100).unwrap(), (10, 20));
        assert_eq!(parse_range("bytes=-10", 100).unwrap(), (90, 99));
        assert!(parse_range("bytes=100-", 100).is_err());
    }
}

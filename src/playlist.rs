use crate::{
    manifest::Track,
    process::{Tools, capture},
};
use anyhow::{Context, Result, bail, ensure};
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
};

pub fn validate_url(url: &str) -> Result<()> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .context("Expected an HTTP(S) YouTube playlist URL")?;
    let host = rest
        .split(['/', '?', '#'])
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    ensure!(
        ["music.youtube.com", "www.youtube.com", "youtube.com"].contains(&host.as_str()),
        "Expected a YouTube or YouTube Music playlist URL"
    );
    ensure!(
        url.split('?')
            .nth(1)
            .unwrap_or("")
            .split('&')
            .any(|p| p.strip_prefix("list=").is_some_and(|v| !v.is_empty())),
        "Playlist URL must contain a nonempty list= parameter"
    );
    Ok(())
}

pub fn sort_tracks(tracks: &mut [Track]) -> Result<()> {
    tracks.sort_by_key(|track| track.index);
    ensure!(!tracks.is_empty(), "Playlist has no tracks");
    for (position, track) in tracks.iter().enumerate() {
        ensure!(track.index > 0, "Playlist indices must be positive");
        if position > 0 {
            ensure!(
                tracks[position - 1].index != track.index,
                "Duplicate playlist index {}",
                track.index
            );
        }
    }
    Ok(())
}

pub fn parse_playlist(value: &Value, track_dir: &Path) -> Result<Vec<Track>> {
    let entries = value
        .get("entries")
        .and_then(Value::as_array)
        .context("yt-dlp did not return playlist entries")?;
    for count_field in ["playlist_count", "n_entries"] {
        if let Some(count) = value.get(count_field).and_then(Value::as_u64) {
            ensure!(
                count == entries.len() as u64,
                "Playlist metadata reports {count} tracks but returned {}; refusing to silently omit unavailable entries",
                entries.len()
            );
        }
    }
    let mut tracks = Vec::new();
    for (position, entry) in entries.iter().enumerate() {
        ensure!(
            !entry.is_null(),
            "Playlist entry {} is unavailable; refusing to skip it",
            position + 1
        );
        let index = match entry.get("playlist_index") {
            Some(v) if !v.is_null() => {
                u32::try_from(v.as_u64().context("Invalid playlist index")?)?
            }
            _ => u32::try_from(position + 1)?, // Snapshot the API's ordered entry position explicitly.
        };
        let id = entry
            .get("id")
            .and_then(Value::as_str)
            .context("Playlist entry has no video ID; refusing to skip it")?;
        ensure!(
            !id.is_empty()
                && id.len() <= 128
                && id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'),
            "Invalid video ID at playlist index {index}"
        );
        let title = entry.get("title").and_then(Value::as_str).unwrap_or(id);
        tracks.push(Track::new(
            index,
            id.into(),
            title.into(),
            format!("https://www.youtube.com/watch?v={id}"),
            track_dir.join(format!("{index:05}-{id}.mp3")),
        ));
    }
    sort_tracks(&mut tracks)?;
    for (position, track) in tracks.iter().enumerate() {
        ensure!(
            track.index as usize == position + 1,
            "Playlist index gap at {}; unavailable tracks must not be silently omitted",
            position + 1
        );
    }
    Ok(tracks)
}

pub fn snapshot(
    tools: &Tools,
    url: &str,
    dir: &Path,
    cookies: Option<&Path>,
) -> Result<Vec<Track>> {
    let mut command = Command::new(&tools.yt_dlp);
    command.args([
        "--ignore-config",
        "--flat-playlist",
        "--dump-single-json",
        "--yes-playlist",
        "--abort-on-error",
    ]);
    if let Some(cookies) = cookies {
        command.arg("--cookies").arg(cookies);
    }
    command.arg("--").arg(url); // Argument arrays and -- keep shell metacharacters/URLs literal.
    let data = capture(&mut command, "playlist metadata download")?;
    parse_playlist(
        &serde_json::from_slice(&data).context("Invalid yt-dlp playlist JSON")?,
        dir,
    )
}

pub fn download(
    tools: &Tools,
    track: &Track,
    staging: &Path,
    cookies: Option<&Path>,
) -> Result<()> {
    let slot = staging.join(format!("{:05}", track.index));
    fs::create_dir_all(&slot)?;
    let template = slot.join(format!("{:05}-{}.%(ext)s", track.index, track.id));
    let expected = slot.join(
        track
            .path
            .file_name()
            .context("Track path has no filename")?,
    );
    let mut command = Command::new(&tools.yt_dlp);
    command.args([
        "--ignore-config",
        "--no-playlist",
        "--abort-on-error",
        "--no-skip-unavailable-fragments",
        "--extract-audio",
        "--audio-format",
        "mp3",
        "--audio-quality",
        "0",
        "--format",
        "bestaudio/best",
        "--no-simulate",
        "--no-overwrites",
    ]);
    command.arg("--output").arg(template);
    // A bare command name is resolved by yt-dlp using the same PATH. A custom
    // path must be passed explicitly (yt-dlp does not resolve bare names here).
    if Path::new(&tools.ffmpeg).components().count() > 1 {
        command.arg("--ffmpeg-location").arg(&tools.ffmpeg);
    }
    if let Some(cookies) = cookies {
        command.arg("--cookies").arg(cookies);
    }
    command.arg("--").arg(&track.url);
    capture(&mut command, &format!("download of playlist index {} ({})", track.index, track.title))
        .context("Fix YouTube access/network problems and retry with the same work directory; cookies can be supplied using --cookies FILE")?;
    ensure!(
        expected.is_file() && expected.metadata()?.len() > 0,
        "yt-dlp produced no MP3 for index {}; refusing to skip it",
        track.index
    );
    // Validate before committing the download; yt-dlp's intermediate .part files stay in staging.
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
        .arg(&expected);
    let codec = capture(&mut probe, "download validation with FFprobe")?;
    ensure!(
        String::from_utf8_lossy(&codec).trim() == "mp3",
        "Downloaded index {} is not MP3 audio",
        track.index
    );
    fs::rename(&expected, &track.path)
        .with_context(|| format!("Cannot commit downloaded {}", track.path.display()))?;
    Ok(())
}

pub fn import_local(dir: &Path) -> Result<Vec<Track>> {
    let dir = fs::canonicalize(dir).context("Cannot open --local-tracks directory")?;
    let mut tracks = vec![];
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if !path.is_file() {
            continue;
        }
        let ext = path
            .extension()
            .and_then(|x| x.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if !["mp3", "wav", "flac"].contains(&ext.as_str()) {
            continue;
        }
        let stem = path
            .file_stem()
            .and_then(|x| x.to_str())
            .context("Local track filename must be UTF-8")?;
        let prefix: String = stem.chars().take_while(char::is_ascii_digit).collect();
        let index: u32 = prefix.parse().with_context(|| {
            format!(
                "{} needs a numeric playlist-index prefix (e.g. 00001-title.mp3)",
                path.display()
            )
        })?;
        if prefix.len() == stem.len() {
            bail!(
                "Local track {} needs a title after its index",
                path.display()
            );
        }
        let mut track = Track::new(
            index,
            format!("local-{index}"),
            stem.into(),
            String::new(),
            PathBuf::from(&path),
        );
        track.downloaded = true;
        tracks.push(track);
    }
    sort_tracks(&mut tracks)?;
    Ok(tracks)
}

# playlist-mix

Download a YouTube Music playlist with **yt-dlp** and turn it into one high-quality
MP3 mix. Rust handles beat detection; FFmpeg handles audio processing. No Python
setup is needed for the Rust tool or analyzer.

## Setup

On macOS, run these commands from this folder:

```sh
brew install mise yt-dlp ffmpeg
mise trust
mise install rust
mise run build
./target/release/playlist-mix models
```

The last command downloads and verifies the beat-analysis models (~83 MB).
Rust and Cargo dependencies are pinned. yt-dlp, FFmpeg and FFprobe must be on `PATH`.

## Run

Paste your actual playlist URL when prompted:

```sh
printf 'Playlist URL: '
IFS= read -r PLAYLIST_URL
./target/release/playlist-mix mix "$PLAYLIST_URL"
```

No output argument or format flag is needed:

- **Mix:** `out/mix.mp3` (48 kHz, libmp3lame VBR quality 0).
- **Downloads:** `out/mix.work/tracks/`, with playlist-index filenames.
- **Analysis and transitions:** `out/mix.work/manifest.json`.
- **Resume state and PCM cache:** `out/mix.work/` (~23 MB per minute of PCM).

Rerun the same command to resume. Unchanged downloads, analysis and completed mixes
are reused. Each work directory keeps its original playlist snapshot; use a new
work directory for a different playlist. Existing output is protected unless you
explicitly request replacement. Run `./target/release/playlist-mix mix --help` for
additional options.

Tracks stay in playlist order. Transitions align detected beats using a default
one-second equal-power crossfade and may trim intros/tails. Tempo, pitch and speed
never change. Beat detection is an estimate; different BPMs still drift during the
overlap. If no suitable transition exists, the tool stops rather than fading
unaligned audio. The final mix is encoded once.

## Troubleshooting

- **Missing tools/models:** rerun setup and check that executables are on `PATH`.
- **YouTube access errors:** update yt-dlp and follow its access/challenge errors;
  authenticated playlists may need a cookies file. Failed tracks are never skipped.
- **No suitable beats:** review the manifest and the CLI's transition options.
- **Existing output or different playlist:** choose another output/work directory
  using the options listed in help.

Only download audio you have permission to use.

## Tests

```sh
mise run test
mise run lint
```

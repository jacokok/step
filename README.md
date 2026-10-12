# step

Turn a YouTube Music playlist into one beat-aligned MP3 mix. Downloads use yt-dlp;
beat detection runs in Rust; FFmpeg handles mixing. Tempo, pitch and speed stay unchanged.

## Environment

You need `yt-dlp` and `FFmpeg` (including FFprobe).

```sh
# macOS
brew install mise yt-dlp ffmpeg
```

## Install

Install the prebuilt CLI from [GitHub Releases](https://github.com/jacokok/step/releases)
using mise's GitHub backend. No Rust or Python project setup is required.

```sh
mise use -g github:jacokok/step
step models
step all "PLAYLIST_URL"
```

`step models` downloads and verifies the analysis models once (~83 MB), storing them
in your user cache directory so they are available from any folder.

## Run

```sh
step all "$PLAYLIST_URL"
```

Your mix is saved to **`out/mix.mp3`**. Ordered downloads, analysis, transitions and
resume state live in **`out/mix.work/`**. Rerun the same command to resume without
re-downloading or re-analyzing unchanged tracks. Use a new work directory for another
playlist; `step all --help` lists the options.

## Separate steps

```sh
step get "$URL"           # Download only, into out/mix.work/tracks/
step cut --threshold -50  # Optional: trim edge silence (dBFS)
step beat                 # Analyze beats only
step mix                  # Plan transitions and render out/mix.mp3
```

`step get` accepts any media or playlist URL supported by yt-dlp, including
YouTube, YouTube Music, and other supported sites. Each new URL appends tracks to
the same collection, using the next available indices without overwriting existing
or manually added tracks:

```sh
step get "https://www.youtube.com/watch?v=VIDEO_ID"
step get "https://music.youtube.com/watch?v=ANOTHER_ID"
step get "https://vimeo.com/VIDEO_ID"
step beat
step mix
```

Rerunning an input resumes its original snapshot and reuses completed downloads;
overlapping inputs reuse tracks with the same resolved media URL. Pass the same
`--work-dir` on every command to use a different collection. Downloading requires
no beat models. Only download media you have permission to use.

`step all` runs the original download → analyze → mix workflow in one command.
`step mix` requires current beat analysis and never downloads tracks or runs the
beat detector. If audio has changed, run `step beat` again first.

Between steps, you can edit tracks in an external program or add your own MP3,
WAV, or FLAC files to `out/mix.work/tracks/`. Filenames must start with a unique,
positive numeric index followed by a title (for example, `00003-my-song.wav`);
indices determine mix order. Remove the original if replacing it with a different
file format, to avoid duplicate indices. Run `step beat` after changes.

### Interactive track cuts

```sh
step edit                              # Downloaded tracks
step edit --local-tracks ./tracks --work-dir ./session
```

Open the local URL printed in the terminal. Switch between tracks with the selector
or **N/P**. A waveform and audio player let you listen and select cuts. Drag on the
waveform, or seek and press **I/O** to mark the section to remove. **Select intro**
and **Select tail** select everything before/after the playhead. **Delete** removes
the selection immediately, joining the remaining audio with FFmpeg; originals are
backed up automatically. A processing indicator stays visible until the edited
waveform and player are ready. Deletion also works while the track selector or
snapping checkbox has focus. Empty selections and the entire track cannot be cut.

**Space** plays/pauses; **Left/Right** seeks 0.1 seconds (**Shift**: 5 seconds);
**Home/End** seeks to track edges; **+/−** zooms around the playhead; **Esc** clears
the selection. Click the waveform to focus keyboard controls. Drag the selection's
boundary handles to refine a cut, zooming in for precision. The bottom icon toolbar
provides play/pause, start/end markers, selection preview, options and deletion.
The options menu contains beat snapping and intro/tail selection.

Toggle beat snapping with **S** or the checkbox. Selection boundaries snap to
previously detected beats (and track edges). Run `step beat` before opening the
editor to enable snapping. Manual cuts preserve markers outside the removed section
and shift them onto the edited timeline, keeping snapping enabled across cuts.
Markers near a new join are estimates; run `step beat` afterward before mixing.
External audio changes still discard stale markers. No beat models are required
for editing or playback.
The waveform is rendered locally in a canvas; no external scripts are loaded.

Cuts retain filenames/order and use the same atomic replacement, original backups,
and `manifest.json` history as `step cut`. MP3 cuts re-encode lossy audio; WAV/FLAC
retain their formats. To undo, copy the recorded backup over the corresponding track.
The editor holds the workflow lock: stop it with **Ctrl+C** in the terminal before
running `step beat` and `step mix`. `--port` optionally sets the loopback server port.

### Trim silence

```sh
step cut                       # Default threshold: -50 dBFS
step cut --threshold -40        # More aggressive: treats louder audio as silence
step cut --threshold -60        # More conservative
step cut --min-silence 0.25     # Only trim edges with at least 250 ms of silence
step cut --threshold -45 --dry-run  # Preview without replacing files
step beat
step mix
```

`cut` uses FFmpeg to decode audio and measures sample peaks in both channels;
it does not use beat models or need yt-dlp. Only continuous silence at the start
and end is trimmed. Internal pauses, entirely silent tracks, and edges shorter
than `--min-silence` (default 0.1 seconds) remain untouched. The threshold must be
a negative dBFS number. Quiet music can fall below the threshold too—preview first.

Files are replaced only after successful encoding and validation, with originals
saved in `<work-dir>/originals/`. Filenames and order are retained. MP3 files are
re-encoded (lossy); WAV/FLAC stay in their respective formats. Cut timestamps and
backup paths are recorded in `manifest.json`. Cuts invalidate beat analysis, so
run `step beat` afterward. `step all` does **not** cut automatically.

Use `--work-dir` and `--local-tracks` with `cut` just as with `beat` and `mix`.
Backups can be copied over the corresponding track to undo a cut; then rerun `beat`.

For an entirely local collection (no playlist URL or yt-dlp needed):

```sh
step beat --local-tracks ./tracks --work-dir ./session
step mix out/local.mp3 --local-tracks ./tracks --work-dir ./session
```

Pass the same `--work-dir` to each step when using a custom session. If tracks are
outside `<work-dir>/tracks`, also pass the same `--local-tracks` to `beat` and `mix`.
`beat` and `mix` synchronize the current local file list, including additions and
removals, and reuse cached analysis for unchanged tracks.

Transitions use a one-second equal-power crossfade and may trim intros/tails. Beat
detection is an estimate: different BPMs still drift during the overlap. Missing
tracks or unsuitable transitions stop the run rather than being silently skipped.

For YouTube access errors, update yt-dlp and follow its instructions; authenticated
playlists may need cookies. Only download audio you have permission to use.

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
step mix "PLAYLIST_URL"
```

`step models` downloads and verifies the analysis models once (~83 MB), storing them
in your user cache directory so they are available from any folder.

## Run

```sh
step mix "$PLAYLIST_URL"
```

Your mix is saved to **`out/mix.mp3`**. Ordered downloads, analysis, transitions and
resume state live in **`out/mix.work/`**. Rerun the same command to resume without
re-downloading or re-analyzing unchanged tracks. Use a new work directory for another
playlist; `step mix --help` lists the options.

Transitions use a one-second equal-power crossfade and may trim intros/tails. Beat
detection is an estimate: different BPMs still drift during the overlap. Missing
tracks or unsuitable transitions stop the run rather than being silently skipped.

For YouTube access errors, update yt-dlp and follow its instructions; authenticated
playlists may need cookies. Only download audio you have permission to use.

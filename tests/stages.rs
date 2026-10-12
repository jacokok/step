use std::process::Command;

#[test]
fn standalone_commands_need_no_playlist_url_and_all_still_requires_one() {
    for name in ["get", "beat", "mix", "all"] {
        let help = Command::new(env!("CARGO_BIN_EXE_step"))
            .args([name, "--help"])
            .output()
            .unwrap();
        assert!(help.status.success());
        let text = String::from_utf8(help.stdout).unwrap();
        assert_eq!(text.contains("<PLAYLIST_URL>"), name == "all");
        assert_eq!(text.contains("<URL>"), name == "get");
    }
    for name in ["get", "all"] {
        let result = Command::new(env!("CARGO_BIN_EXE_step"))
            .arg(name)
            .output()
            .unwrap();
        assert!(!result.status.success());
        let expected = if name == "get" {
            "<URL>"
        } else {
            "<PLAYLIST_URL>"
        };
        assert!(String::from_utf8_lossy(&result.stderr).contains(expected));
    }
}

#[cfg(unix)]
#[test]
fn get_downloads_and_resumes_without_models_pcm_or_transitions() {
    use std::{fs, os::unix::fs::PermissionsExt};
    use step::{
        manifest::Manifest,
        pipeline::{self, GetArgs},
        process::Tools,
    };
    let dir = tempfile::tempdir().unwrap();
    let tool = dir.path().join("fake-tool");
    fs::write(
        &tool,
        r#"#!/bin/sh
case "$1" in
  --version|-version) echo fake-version; exit 0 ;;
  -v) echo mp3; exit 0 ;;
esac
metadata=no
url=
while [ "$#" -gt 0 ]; do
  case "$1" in
    --dump-single-json) metadata=yes ;;
    --output) shift; template=$1 ;;
    https://*) url=$1 ;;
  esac
  shift
done
if [ "$metadata" = yes ]; then
  case "$url" in
    *list=TEST*) echo '{"entries":[{"id":"first","title":"First"}]}' ;;
    *list=MORE*) echo '{"entries":[{"id":"third","url":"https://example.org/third","playlist_index":2},{"id":"first","url":"https://www.youtube.com/watch?v=first","playlist_index":1}]}' ;;
    *) printf '{"id":"provider/id: unsafe %% name","title":"Other","webpage_url":"%s"}\n' "$url" ;;
  esac
else
  if [ "$url" = https://example.org/fail ] && [ -f "$(dirname "$0")/fail" ]; then
    echo 'simulated download failure' >&2
    exit 42
  fi
  echo "$url" >> "$(dirname "$0")/downloads"
  output=${template%'%(ext)s'}mp3
  printf audio > "$output"
fi
"#,
    )
    .unwrap();
    fs::set_permissions(&tool, fs::Permissions::from_mode(0o755)).unwrap();
    let tools = Tools {
        yt_dlp: tool.clone().into_os_string(),
        ffmpeg: tool.clone().into_os_string(),
        ffprobe: tool.into_os_string(),
    };
    let work = dir.path().join("work");
    let invoke = || {
        pipeline::get(
            GetArgs {
                url: "https://youtube.com/playlist?list=TEST".into(),
                work_dir: Some(work.clone()),
                cookies: None,
            },
            &tools,
        )
    };
    invoke().unwrap();
    let saved = Manifest::load(&work.join("manifest.json")).unwrap();
    assert!(saved.tracks[0].downloaded);
    assert!(saved.tracks[0].analysis.is_none());
    assert!(saved.tracks[0].pcm_path.is_none());
    assert!(saved.transitions.is_empty());
    assert!(saved.output.is_none());
    assert!(!work.join("pcm").exists());
    assert!(!work.join("filtergraph.txt").exists());
    // If get downloads again, this sentinel will be replaced.
    fs::write(&saved.tracks[0].path, "unchanged").unwrap();
    invoke().unwrap();
    assert_eq!(
        fs::read_to_string(&saved.tracks[0].path).unwrap(),
        "unchanged"
    );
    let get_url = |url: &str| {
        pipeline::get(
            GetArgs {
                url: url.into(),
                work_dir: Some(work.clone()),
                cookies: None,
            },
            &tools,
        )
    };
    get_url("https://vimeo.com/123").unwrap();
    get_url("https://music.youtube.com/watch?v=music").unwrap();
    get_url("https://youtube.com/playlist?list=MORE").unwrap();
    let saved = Manifest::load(&work.join("manifest.json")).unwrap();
    assert_eq!(
        saved.tracks.iter().map(|t| t.index).collect::<Vec<_>>(),
        vec![1, 2, 3, 4]
    );
    assert_eq!(saved.tracks[1].url, "https://vimeo.com/123");
    assert_eq!(
        saved.tracks[2].url,
        "https://music.youtube.com/watch?v=music"
    );
    assert_eq!(saved.tracks[3].url, "https://example.org/third");
    assert!(
        saved
            .tracks
            .iter()
            .all(|t| t.path.parent() == Some(work.join("tracks").as_path()))
    );
    assert!(saved.tracks.iter().all(|t| t.path.is_file()));
    // Overlapping playlists and repeated inputs must not duplicate or replace audio.
    get_url("https://vimeo.com/123").unwrap();
    assert_eq!(
        fs::read_to_string(dir.path().join("downloads"))
            .unwrap()
            .lines()
            .count(),
        4
    );
    assert_eq!(
        fs::read_to_string(&saved.tracks[0].path).unwrap(),
        "unchanged"
    );

    // User-supplied track indices are reserved before choosing the next index.
    let own = work.join("tracks/00010-own.wav");
    fs::write(&own, "own audio").unwrap();
    get_url("https://example.org/next").unwrap();
    let saved = Manifest::load(&work.join("manifest.json")).unwrap();
    assert_eq!(saved.tracks.last().unwrap().index, 11);
    assert_eq!(fs::read_to_string(&own).unwrap(), "own audio");

    // Save pending tracks before downloading; retry the same URL at the same index.
    fs::write(dir.path().join("fail"), "").unwrap();
    assert!(get_url("https://example.org/fail").is_err());
    let saved = Manifest::load(&work.join("manifest.json")).unwrap();
    assert_eq!(saved.tracks.last().unwrap().index, 12);
    assert!(!saved.tracks.last().unwrap().downloaded);
    assert!(
        saved
            .last_error
            .unwrap()
            .contains("simulated download failure")
    );
    fs::remove_file(dir.path().join("fail")).unwrap();
    get_url("https://example.org/fail").unwrap();
    let saved = Manifest::load(&work.join("manifest.json")).unwrap();
    assert_eq!(saved.tracks.len(), 7);
    assert_eq!(saved.tracks.last().unwrap().index, 12);
    assert!(saved.tracks.last().unwrap().downloaded);
    assert!(saved.last_error.is_none());
}

#[test]
fn generic_metadata_preserves_provider_urls_and_rejects_unavailable_entries() {
    use serde_json::json;
    use step::playlist::parse_input;
    let single = parse_input(
        &json!({"id":"a/b: unsafe", "title":"Single", "url":"https://cdn.example.org/signed"}),
        "https://example.org/page",
    )
    .unwrap();
    assert_eq!(single[0].url, "https://example.org/page");
    let tracks = parse_input(&json!({"entries":[
        {"id":"b", "url":"https://vimeo.com/b", "playlist_index":2},
        {"id":"a", "webpage_url":"https://example.org/a", "url":"https://cdn.example.org/a", "playlist_index":1}
    ]}), "https://example.org/playlist").unwrap();
    assert_eq!(tracks[0].url, "https://example.org/a");
    assert_eq!(tracks[1].url, "https://vimeo.com/b");
    assert!(parse_input(&json!({"entries":[null]}), "https://example.org/list").is_err());
    assert!(parse_input(&json!({"entries":[]}), "https://example.org/list").is_err());
    assert!(
        parse_input(
            &json!({"entries":[{"id":"missing-url"}]}),
            "https://example.org/list"
        )
        .is_err()
    );
    assert!(
        parse_input(
            &json!({"playlist_count":2,"entries":[{"id":"a", "url":"https://example.org/a"}]}),
            "https://example.org/list"
        )
        .is_err()
    );
    let tracks = parse_input(
        &json!({"entries":[{"url":"https://example.org/no-id"}]}),
        "https://example.org/list",
    )
    .unwrap();
    assert_eq!(tracks[0].url, "https://example.org/no-id");
    for invalid in [
        json!({"entries":null}),
        json!({"_type":"playlist"}),
        json!("invalid"),
    ] {
        assert!(parse_input(&invalid, "https://example.org/list").is_err());
    }
    let nested = parse_input(
        &json!({"entries":[{"entries":[
            {"id":"one", "webpage_url":"https://example.org/one"},
            {"id":"two", "webpage_url":"https://example.org/two"}
        ]}]}),
        "https://example.org/list",
    )
    .unwrap();
    assert_eq!(
        nested.iter().map(|t| t.index).collect::<Vec<_>>(),
        vec![1, 2]
    );
}

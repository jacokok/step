use serde_json::{Value, json};
use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    net::TcpStream,
    path::Path,
    process::{Child, Command, Stdio},
    time::Duration,
};
use step::{
    SAMPLE_RATE, analysis,
    manifest::{Analysis, Beat, Manifest},
    playlist,
    process::Tools,
};

struct Editor {
    child: Child,
    host: String,
    token: String,
}
impl Drop for Editor {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
impl Editor {
    fn start(work: &Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_step"))
            .args(["edit", "--work-dir"])
            .arg(work)
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        let host = line
            .strip_prefix("Open http://")
            .unwrap()
            .split_whitespace()
            .next()
            .unwrap()
            .to_owned();
        let mut editor = Self {
            child,
            host,
            token: String::new(),
        };
        let (_, html) = editor.request("GET", "/", None, "");
        let html = String::from_utf8(html).unwrap();
        editor.token = html
            .split("const token='")
            .nth(1)
            .unwrap()
            .split('\'')
            .next()
            .unwrap()
            .to_owned();
        editor
    }
    fn request(
        &self,
        method: &str,
        path: &str,
        body: Option<Value>,
        extra: &str,
    ) -> (String, Vec<u8>) {
        let mut stream = TcpStream::connect(&self.host).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(15)))
            .unwrap();
        let body = body.map(|b| b.to_string()).unwrap_or_default();
        write!(stream,"{method} {path} HTTP/1.1\r\nHost: {}\r\nContent-Length: {}\r\nX-Step-Token: {}\r\n{extra}\r\n{body}",self.host,body.len(),self.token).unwrap();
        let mut bytes = vec![];
        stream.read_to_end(&mut bytes).unwrap();
        let split = bytes.windows(4).position(|s| s == b"\r\n\r\n").unwrap();
        (
            String::from_utf8(bytes[..split].to_vec()).unwrap(),
            bytes[split + 4..].to_vec(),
        )
    }
    fn get(&self, path: &str) -> Value {
        let (headers, body) = self.request("GET", path, None, "");
        assert!(
            headers.starts_with("HTTP/1.1 200"),
            "{headers}: {:?}",
            String::from_utf8_lossy(&body)
        );
        serde_json::from_slice(&body).unwrap()
    }
}
fn wav(path: &Path) {
    let mut writer = hound::WavWriter::create(
        path,
        hound::WavSpec {
            channels: 2,
            sample_rate: SAMPLE_RATE,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        },
    )
    .unwrap();
    for i in 0..SAMPLE_RATE {
        for _ in 0..2 {
            writer
                .write_sample(if i < 12000 {
                    0.25f32
                } else if i < 36000 {
                    -0.5
                } else {
                    0.75
                })
                .unwrap();
        }
    }
    writer.finalize().unwrap();
}

#[test]
fn editor_serves_waveform_playback_and_safely_removes_middle_and_edges() {
    let dir = tempfile::tempdir().unwrap();
    let work = dir.path();
    let tracks = work.join("tracks");
    fs::create_dir(&tracks).unwrap();
    let source = tracks.join("00001-song.wav");
    wav(&source);
    wav(&tracks.join("00002-other.wav"));
    let original = fs::read(&source).unwrap();
    let tools = Tools::default();
    let versions = tools.check(true).unwrap();
    let mut imported = playlist::import_local(&tracks).unwrap();
    analysis::prepare_pcm(
        &mut imported[0],
        &work.join("pcm"),
        &tools,
        &versions["ffmpeg"],
    )
    .unwrap();
    imported[0].analysis = Some(Analysis {
        cache_key: "fixture".into(),
        detector: "fixture".into(),
        bpm: Some(120.0),
        confidence: 1.0,
        confidence_kind: "fixture".into(),
        interval_regularity: 1.0,
        beats: vec![
            Beat {
                seconds: 0.25,
                sample: 12000,
                score: 1.0,
            },
            Beat {
                seconds: 0.75,
                sample: 36000,
                score: 1.0,
            },
        ],
        downbeats: vec![0.25, 0.75],
        warnings: vec![],
    });
    let mut manifest = Manifest::new(
        String::new(),
        format!("local:{}", tracks.display()),
        imported,
    );
    manifest.output_plan_key = Some("old-plan".into());
    manifest.save(&work.join("manifest.json")).unwrap();
    let mut editor = Editor::start(work);
    assert_eq!(editor.get("/api/tracks").as_array().unwrap().len(), 2);
    assert_eq!(editor.get("/api/track/2")["index"], 2);
    let detail = editor.get("/api/track/1");
    assert_eq!(detail["duration"], 1.0);
    assert_eq!(detail["beats"], json!([0.25, 0.75]));
    assert_eq!(detail["peaks"].as_array().unwrap().len(), 2400);
    assert_eq!(detail["peaks"][0], 0.25);
    assert_eq!(detail["peaks"][2399], 0.75);
    let (headers, audio) = editor.request("GET", "/audio/1", None, "Range: bytes=0-99\r\n");
    assert!(headers.contains("206 Partial Content"));
    assert_eq!(audio.len(), 100);
    assert_eq!(&audio[..4], b"RIFF");
    let payload = json!({"index":1,"hash":detail["hash"],"start":0.25,"end":0.75});
    let (_, denied) = editor.request(
        "POST",
        "/api/remove",
        Some(payload.clone()),
        "Origin: https://evil.example\r\n",
    );
    assert!(
        serde_json::from_slice::<Value>(&denied).unwrap()["error"]
            .as_str()
            .unwrap()
            .contains("Origin")
    );
    let token = std::mem::take(&mut editor.token);
    let (_, denied) = editor.request("POST", "/api/remove", Some(payload.clone()), "");
    assert!(
        serde_json::from_slice::<Value>(&denied).unwrap()["error"]
            .as_str()
            .unwrap()
            .contains("token")
    );
    editor.token = token;
    let (_, denied) = editor.request(
        "POST",
        "/api/remove",
        Some(json!({"index":1,"hash":"stale","start":0.25,"end":0.75})),
        "",
    );
    assert!(
        serde_json::from_slice::<Value>(&denied).unwrap()["error"]
            .as_str()
            .unwrap()
            .contains("changed")
    );
    let (_, denied) = editor.request(
        "POST",
        "/api/remove",
        Some(json!({"index":1,"hash":detail["hash"],"start":0.,"end":1.})),
        "",
    );
    assert!(
        serde_json::from_slice::<Value>(&denied).unwrap()["error"]
            .as_str()
            .unwrap()
            .contains("entire")
    );
    assert_eq!(fs::read(&source).unwrap(), original);
    let (headers, body) = editor.request("POST", "/api/remove", Some(payload), "");
    assert!(
        headers.contains("200 OK"),
        "{}",
        String::from_utf8_lossy(&body)
    );
    let result: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        fs::read(result["backup"].as_str().unwrap()).unwrap(),
        original
    );
    let samples: Vec<f32> = hound::WavReader::open(&source)
        .unwrap()
        .samples()
        .map(Result::unwrap)
        .collect();
    assert_eq!(samples.len(), 48000);
    assert!(samples[..24000].iter().all(|s| *s == 0.25));
    assert!(samples[24000..].iter().all(|s| *s == 0.75));
    let manifest = Manifest::load(&work.join("manifest.json")).unwrap();
    assert_eq!(
        manifest.tracks[0].analysis.as_ref().unwrap().beats[0].sample,
        12000
    );
    assert!(manifest.tracks[0].pcm_path.is_some());
    let edited = manifest.tracks[0].analysis.as_ref().unwrap();
    assert_eq!(edited.downbeats, vec![0.25]);
    assert_ne!(
        edited.cache_key,
        analysis::cache_key(&manifest.tracks[0], "fixture").unwrap(),
        "shifted editing markers must not bypass fresh analysis for mixing"
    );
    assert!(manifest.output_plan_key.is_none());
    assert_eq!(manifest.config["cut_history"].as_array().unwrap().len(), 1);
    // Repeated edge cuts use fresh hashes and operate on the remaining audio.
    let detail = editor.get("/api/track/1");
    assert_eq!(detail["duration"], 0.5);
    assert_eq!(detail["beats"], json!([0.25]));
    let (h, _) = editor.request(
        "POST",
        "/api/remove",
        Some(json!({"index":1,"hash":detail["hash"],"start":0.,"end":0.125})),
        "",
    );
    assert!(h.contains("200 OK"));
    let detail = editor.get("/api/track/1");
    assert_eq!(detail["duration"], 0.375);
    assert_eq!(detail["beats"], json!([0.125]));
    let (h, _) = editor.request(
        "POST",
        "/api/remove",
        Some(json!({"index":1,"hash":detail["hash"],"start":0.25,"end":0.375})),
        "",
    );
    assert!(h.contains("200 OK"));
    assert_eq!(editor.get("/api/track/1")["duration"], 0.25);
    // The workflow lock prevents another stage modifying files mid-session.
    let cut = Command::new(env!("CARGO_BIN_EXE_step"))
        .args(["cut", "--work-dir"])
        .arg(work)
        .output()
        .unwrap();
    assert!(!cut.status.success());
    assert!(String::from_utf8_lossy(&cut.stderr).contains("locked"));
    // Only our known cuts can reuse markers; unrelated source changes cannot.
    wav(&source);
    assert_eq!(editor.get("/api/track/1")["beats"], json!([]));
}

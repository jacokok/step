use std::{fs, process::Command};
use step::{
    SAMPLE_RATE,
    manifest::Track,
    process::Tools,
    render::{self, Format},
};

#[test]
fn encoder_failure_does_not_replace_existing_output() {
    if !Command::new("ffmpeg")
        .arg("-version")
        .output()
        .is_ok_and(|o| o.status.success())
    {
        eprintln!("SKIP: install FFmpeg to run output-protection test");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("mix.flac");
    let sentinel = b"existing output must survive a failed replacement";
    fs::write(&output, sentinel).unwrap();
    let mut t = Track::new(
        1,
        "fixture".into(),
        "fixture".into(),
        String::new(),
        dir.path().join("missing.mp3"),
    );
    t.frames = Some(SAMPLE_RATE as u64);
    t.pcm_path = Some(dir.path().join("missing.f32"));
    let error = render::render(
        &Tools::default(),
        &[t],
        &[],
        &output,
        &dir.path().join("filtergraph.txt"),
        Format::Flac,
        true,
    )
    .unwrap_err();
    assert!(format!("{error:#}").contains("final FFmpeg mix/encode failed"));
    assert_eq!(fs::read(&output).unwrap(), sentinel);
    assert!(!dir.path().join(".mix.flac.partial.flac").exists());
}

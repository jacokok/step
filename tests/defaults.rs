use clap::Parser;
use std::path::{Path, PathBuf};
use step::{pipeline::MixArgs, render::Format};

#[derive(Parser)]
struct Arguments {
    #[command(flatten)]
    mix: MixArgs,
}

fn parse(extra: &[&str]) -> MixArgs {
    let mut args = vec!["step", "https://music.youtube.com/playlist?list=provided"];
    args.extend_from_slice(extra);
    Arguments::try_parse_from(args).unwrap().mix
}

#[test]
fn url_alone_defaults_to_mp3_and_output_directory() {
    let args = parse(&[]);
    assert_eq!(args.format, Format::Mp3);
    assert!(args.output.is_none());
    assert_eq!(args.output_path(), PathBuf::from("out/mix.mp3"));
    assert_eq!(
        Format::infer(&args.output_path(), None).unwrap(),
        Format::Mp3
    );
}

#[test]
fn explicit_output_does_not_change_default_encoding() {
    let args = parse(&["custom/music.mp3"]);
    assert_eq!(args.output_path(), PathBuf::from("custom/music.mp3"));
    assert_eq!(args.format, Format::Mp3);
    assert!(Format::infer(Path::new("custom/music.flac"), None).is_err());
}

#[test]
fn explicit_format_changes_default_extension_and_keeps_custom_paths() {
    let args = parse(&["--format", "flac"]);
    assert_eq!(args.format, Format::Flac);
    assert_eq!(args.output_path(), PathBuf::from("out/mix.flac"));
    assert_eq!(
        Format::infer(&args.output_path(), Some(args.format)).unwrap(),
        Format::Flac
    );
    let args = parse(&["custom/music.flac", "--format", "flac"]);
    assert_eq!(args.output_path(), PathBuf::from("custom/music.flac"));
    assert!(Format::infer(Path::new("wrong.mp3"), Some(Format::Flac)).is_err());
}

#[test]
fn playlist_url_remains_required() {
    assert!(Arguments::try_parse_from(["step"]).is_err());
}

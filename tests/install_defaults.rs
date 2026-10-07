use std::{fs, process::Command};

#[test]
fn installed_binary_is_named_step() {
    let output = Command::new(env!("CARGO_BIN_EXE_step"))
        .arg("--version")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap().trim(),
        format!("step {}", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn installed_model_defaults_are_shared_across_working_directories() {
    let dir = tempfile::tempdir().unwrap();
    let first = dir.path().join("first");
    let second = dir.path().join("second");
    fs::create_dir(&first).unwrap();
    fs::create_dir(&second).unwrap();
    let help = |cwd, subcommand| {
        let output = Command::new(env!("CARGO_BIN_EXE_step"))
            .current_dir(cwd)
            .env_remove("STEP_MODEL_DIR")
            .args([subcommand, "--help"])
            .output()
            .unwrap();
        assert!(output.status.success());
        String::from_utf8(output.stdout).unwrap()
    };
    let default_dir = step::models::default_dir();
    assert!(default_dir.is_absolute());
    let first_help = help(&first, "models");
    assert_eq!(first_help, help(&second, "models"));
    assert!(first_help.contains(default_dir.to_str().unwrap()));
    assert!(help(&second, "mix").contains(default_dir.to_str().unwrap()));
}

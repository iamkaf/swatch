use std::process::Command;

fn version_output(flag: &str) -> std::process::Output {
    let directory = tempfile::tempdir().expect("temporary working directory");
    Command::new(env!("CARGO_BIN_EXE_swatch"))
        .arg(flag)
        .current_dir(directory.path())
        .output()
        .expect("run Swatch")
}

#[test]
fn long_version_flag_prints_the_package_version() {
    let output = version_output("--version");
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).expect("UTF-8 stdout"),
        format!("swatch {}\n", env!("CARGO_PKG_VERSION"))
    );
    assert!(output.stderr.is_empty());
}

#[test]
fn short_version_flag_matches_the_long_form() {
    let output = version_output("-V");
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).expect("UTF-8 stdout"),
        format!("swatch {}\n", env!("CARGO_PKG_VERSION"))
    );
    assert!(output.stderr.is_empty());
}

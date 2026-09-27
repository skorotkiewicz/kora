use std::process::Command;

fn run(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_kora"))
        .args(args)
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .env_remove("HOME")
        .output()
        .unwrap()
}

#[test]
fn help_and_version_do_not_need_a_display_or_configuration() {
    let help = run(&["--help"]);
    assert!(help.status.success());
    assert!(String::from_utf8_lossy(&help.stdout).contains("--config"));
    let version = run(&["--version"]);
    assert!(version.status.success());
    assert_eq!(String::from_utf8_lossy(&version.stdout).trim(), concat!("kora ", env!("CARGO_PKG_VERSION")));
}

#[test]
fn invalid_arguments_fail_before_starting_the_desktop() {
    for args in [&["--config"][..], &["--unknown"][..], &["--config", "a", "--config", "b"][..]] {
        let result = run(args);
        assert_eq!(result.status.code(), Some(1));
        assert!(String::from_utf8_lossy(&result.stderr).contains("kora:"));
    }
}

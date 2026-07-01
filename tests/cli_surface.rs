use std::process::Command;

use pretty_assertions::assert_eq;

fn hyprdisjust() -> Command {
    Command::new(env!("CARGO_BIN_EXE_hyprdisjust"))
}

#[test]
fn help_lists_bootstrap_command_surface() {
    let output = hyprdisjust().arg("--help").output().unwrap();

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    for command in ["doctor", "list", "save", "apply", "daemon", "export"] {
        assert!(
            stdout.contains(command),
            "expected help output to include `{command}`:\n{stdout}"
        );
    }
}

#[test]
fn unimplemented_commands_return_clear_errors() {
    for args in [
        vec!["list"],
        vec!["save", "desk"],
        vec!["apply", "desk"],
        vec!["apply", "--auto"],
        vec!["daemon"],
        vec!["export", "--format", "conf"],
        vec!["export", "--format", "lua"],
    ] {
        let output = hyprdisjust().args(args.clone()).output().unwrap();

        assert_eq!(output.status.code(), Some(1), "{args:?}");
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(
            stderr.contains("not implemented yet"),
            "expected not-implemented error for {args:?}, got:\n{stderr}"
        );
    }
}

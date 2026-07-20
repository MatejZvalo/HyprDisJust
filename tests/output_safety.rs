use hyprdisjust::cli::sanitize_terminal_text;
use hyprdisjust::profile::render::format_hyprctl_batch_command;

#[test]
fn terminal_text_cannot_emit_control_sequences() {
    let sanitized = sanitize_terminal_text("safe\u{1b}[31m\nnext");

    assert!(!sanitized.contains('\u{1b}'));
    assert!(!sanitized.contains('\n'));
    assert!(sanitized.contains("\\u{1b}"));
    assert!(sanitized.contains("\\u{a}"));
}

#[test]
fn dangerous_dry_run_values_use_posix_single_argument_quoting() {
    let command = format_hyprctl_batch_command("$(touch /tmp/pwn) `id` ' quote \\");

    assert!(command.starts_with("hyprctl --batch '"));
    assert!(command.contains("'\\''"));
    assert!(!command.contains("--batch \""));
}

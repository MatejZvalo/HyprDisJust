use std::fmt::Write as _;
use std::io::{self, Write as _};

pub fn sanitize_terminal_text(value: &str) -> String {
    sanitize(value, false)
}

pub fn sanitize_multiline_text(value: &str) -> String {
    sanitize(value, true)
}

fn sanitize(value: &str, allow_layout_controls: bool) -> String {
    let mut sanitized = String::with_capacity(value.len());
    for character in value.chars() {
        if character.is_control() && !(allow_layout_controls && matches!(character, '\n' | '\t')) {
            let _ = write!(sanitized, "\\u{{{:x}}}", u32::from(character));
        } else {
            sanitized.push(character);
        }
    }
    sanitized
}

pub fn write_stdout(value: &str) -> io::Result<()> {
    let stdout = io::stdout();
    let mut stdout = stdout.lock();
    stdout.write_all(value.as_bytes())
}

pub fn write_stdout_line(value: &str) -> io::Result<()> {
    let stdout = io::stdout();
    let mut stdout = stdout.lock();
    stdout.write_all(value.as_bytes())?;
    stdout.write_all(b"\n")
}

# HyprDisJust

HyprDisJust is a Hyprland monitor profile manager. The idea is to save monitor layouts and re-apply them automatically when the same monitors are connected again.

## Quickstart

```text
cargo install --path .
hyprdisjust doctor
hyprdisjust save laptop
hyprdisjust save desk
hyprdisjust apply desk --dry-run
hyprdisjust install-systemd-user --enable --start
```

Run `doctor` first. It prints Hyprland session status, config paths, saved
profile count, generated config paths, socket2 availability, stale output-name
hints, and the best automatic profile decision.

## Status

Current usable commands:

- `hyprdisjust doctor`, which reads the current Hyprland monitor state and
  prints diagnostics plus normalized monitor identities
- `hyprdisjust save [name]`, which saves the current layout to
  `~/.config/hyprdisjust/profiles.toml`
- `hyprdisjust rename <old> <new>`, which renames a saved profile
- `hyprdisjust delete <name> --yes`, which removes a saved profile
- `hyprdisjust copy <source> <dest>`, which duplicates a saved profile
- `hyprdisjust list`, which lists saved profiles
- `hyprdisjust apply <name>`, which applies a saved profile to the current
  matching outputs
- `hyprdisjust apply --auto`, which applies the best exact or high-confidence
  saved profile match
- `hyprdisjust apply <name> --dry-run` and `hyprdisjust apply --auto --dry-run`,
  which print the exact `hyprctl --batch` command without changing monitor
  layout
- `hyprdisjust daemon --once --dry-run`, which runs one automatic selection
  decision and prints the generated command without changing monitor layout
- `hyprdisjust daemon`, which watches Hyprland socket2 monitor hot-plug events
  and applies exact or high-confidence saved profile matches
- `hyprdisjust export [name] --format lua`, which writes generated Lua monitor
  calls to `~/.config/hyprdisjust/generated/monitors.lua`
- `hyprdisjust tui`, which opens a terminal editor for current monitors,
  saved profiles, generated apply commands, and layout warnings
- `hyprdisjust install-systemd-user`, which writes a user service for the
  daemon
- `hyprdisjust completions <bash|zsh|fish>`, which prints shell completion
  setup

When export is run without a profile name, HyprDisJust uses the same automatic
selection rules as `apply --auto`. Named and automatic export both query the
currently connected monitors so saved physical monitor identities can be mapped
to the current Hyprland output names.

Apply, export, daemon, and TUI use a shared apply plan that maps saved physical
monitor identities to current output names, validates obviously invalid saved
output values, and reports layout warnings such as overlapping enabled outputs.
Live apply refuses profiles that would disable every saved output.

Aiming for a small Rust CLI and TUI first. Anything fancier can come later if
the basic workflow proves useful.

## Goals

- Save the current Hyprland monitor layout
- Match monitors by something stable like description
- Re-apply a saved layout when that monitor setup appears again
- Get functioning hot-plug

## Development

```text
cargo fmt --check
cargo test
cargo clippy --all-targets --all-features -- -D warnings
cargo run -- --help
cargo run -- doctor
cargo run -- save desk
cargo run -- list
cargo run -- rename desk work
cargo run -- copy work backup
cargo run -- delete backup --yes
cargo run -- apply desk --dry-run
cargo run -- apply --auto --dry-run
cargo run -- daemon --once --dry-run
cargo run -- export desk --format lua
cargo run -- export --format lua
cargo run -- tui
cargo run -- install-systemd-user --dry-run
cargo run -- completions bash
```

## Config

HyprDisJust reads optional config from `~/.config/hyprdisjust/config.toml`.
Currently supported:

```toml
fallback_profile = "laptop"
debounce_ms = 900
apply_on_start = false
tui_move_step = 20
```

The fallback profile is used only when no exact or high-confidence profile
match is found.

The daemon waits `debounce_ms` after monitor hot-plug events before querying
Hyprland again. `apply_on_start` is disabled by default so starting the daemon
does not immediately change monitor layout.

The TUI moves the selected monitor by `tui_move_step` logical pixels when using
the arrow-key nudge controls. The TUI shows this configured step in its action
area.

## Daemon Install

Install the foreground daemon as a systemd user service:

```text
hyprdisjust install-systemd-user --enable --start
```

This writes `~/.config/systemd/user/hyprdisjust.service` with an `ExecStart`
pointing at the current `hyprdisjust` executable. Use `--dry-run` to inspect the
service file without writing or calling `systemctl`.

After changing the installed binary path, rerun `install-systemd-user`. Useful
manual checks:

```text
systemctl --user status hyprdisjust.service
journalctl --user -u hyprdisjust.service
hyprdisjust daemon --once --dry-run
```

## Generated Hyprland Lua

HyprDisJust writes generated files under
`~/.config/hyprdisjust/generated/`. It never edits Hyprland's own config.

Lua export writes `hl.monitor({...})` calls to `monitors.lua`. The exact
require/import shape depends on the Lua runtime used by your Hyprland setup.

## Recovery Notes

- `hyprdisjust apply <name> --dry-run` prints the exact `hyprctl --batch`
  command before changing anything. Live apply uses Hyprland's `eval`
  `hl.monitor(...)` form so it works with the newer non-legacy config parser.
- If an apply fails, HyprDisJust prints the previous monitor layout to help
  recover manually.
- `hyprdisjust doctor` reports stale output-name hints when a dock or adapter
  renamed outputs but physical identities still match.
- Generated Lua files live under `~/.config/hyprdisjust/generated/`;
  HyprDisJust does not edit `hyprland.conf`.
- Delete and overwrite operations require explicit confirmation flags or TUI
  confirmation.

## Shell Completions

Example:

```text
hyprdisjust completions bash > ~/.local/share/bash-completion/completions/hyprdisjust
hyprdisjust completions zsh > ~/.zfunc/_hyprdisjust
hyprdisjust completions fish > ~/.config/fish/completions/hyprdisjust.fish
```

## Release / Install

For local use:

```text
cargo install --path .
```

For packaging, build with:

```text
cargo build --release
```

Ship the binary plus the command reference in `docs/COMMANDS.md`. The runtime
requirements are Hyprland, `hyprctl`, and a user session where
`XDG_RUNTIME_DIR` and `HYPRLAND_INSTANCE_SIGNATURE` are available.

## License

No license for now.

# HyprDisJust

HyprDisJust is a Hyprland monitor profile manager. The idea is to save monitor layouts and re-apply them automatically when the same monitors are connected again.

## Quickstart

```text
cargo install --path .
hyprdisjust doctor
hyprdisjust save laptop
hyprdisjust save desk
hyprdisjust apply desk --dry-run
hyprdisjust apply desk
hyprdisjust install-systemd-user --unattended --enable --start
```

Run `doctor` first. It prints Hyprland session status, config paths, saved
profile count, generated config paths, socket2 availability, stale output-name
hints, systemd installation guidance, and the best automatic profile decision
with its score and reason.
It exits nonzero when any check has error severity; a successful empty query is
reported as a detected Hyprland session with zero monitors.

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
  matching outputs and requires a post-apply `y` confirmation within 15 seconds
- `hyprdisjust apply <name> --unattended`, which deliberately bypasses the
  post-apply confirmation for automation while retaining apply-failure recovery
- `hyprdisjust apply --auto`, which applies the best exact or high-confidence
  saved profile match
- `hyprdisjust apply <name> --dry-run` and `hyprdisjust apply --auto --dry-run`,
  which print the exact `hyprctl --batch` command without changing monitor
  layout
- `hyprdisjust daemon --once --dry-run`, which runs one automatic selection
  decision and prints the generated command without changing monitor layout
- `hyprdisjust daemon`, which watches Hyprland socket2 monitor hot-plug events
  and applies exact or high-confidence saved profile matches; each changed
  layout requires interactive confirmation
- `hyprdisjust daemon --unattended`, which explicitly enables automatic applies
  without confirmation for a service or other non-interactive session
- `hyprdisjust export [name] --format lua`, which writes generated Lua monitor
  calls to `~/.config/hyprdisjust/generated/monitors.lua`
- `hyprdisjust tui`, which opens a terminal editor for current monitors,
  saved profiles, profile CRUD, output settings, generated apply commands,
  automatic selection, refresh, help, and layout warnings
- `hyprdisjust install-systemd-user`, which writes a user service for the
  daemon
- `hyprdisjust completions <bash|elvish|fish|powershell|zsh>`, which prints shell completion
  setup

When export is run without a profile name, HyprDisJust uses the same automatic
selection rules as `apply --auto`. Named and automatic export both query the
currently connected monitors so saved physical monitor identities can be mapped
to the current Hyprland output names.

Apply, export, daemon, and TUI use a shared apply plan that maps saved physical
monitor identities to current output names, validates obviously invalid saved
output values, and reports layout warnings such as overlapping enabled outputs.
Live apply refuses profiles that would disable every saved output. It configures
desired active outputs before disabling obsolete ones, verifies the resulting
state, and restores the captured layout if execution or verification fails.
Already-active plans are detected before execution, so CLI and daemon runs do
not needlessly reapply the same effective layout.

Every live mutation ignores `HYPRDISJUST_MONITORS_JSON` and enters one per-user
serialized transaction before querying the compositor. Automatic selection is
repeated under that lock, and the lock remains held through planning, apply,
convergence, confirmation, verification, and rollback. Verification rejects
added, removed, renamed, or identity-changed outputs as topology drift and uses
a bounded convergence window of roughly three seconds. The fixture
environment variable remains available for read-only CLI and save tests only.

After a changed layout is applied, HyprDisJust shows a 15-second countdown.
Press lowercase `y` to keep it. Timeout, any other key, closed input, or an input
read error triggers restoration of the normalized monitor configuration captured
immediately before apply. A rollback is verified against that captured state;
both apply and rollback failures include the underlying `hyprctl` or convergence
error. Non-terminal input is rejected before changing outputs unless the command
has the explicit `--unattended` flag. TUI apply uses the same post-apply safety
transaction.

If an output was disabled before apply, rollback briefly restores its captured
mode, position, scale, and transform before disabling it again. This prevents a
disable-only rollback from retaining settings introduced by the rejected layout.

Automatic matching uses a deterministic global one-to-one mapping. Exact
stable physical IDs win; connector-only and connector-disambiguated IDs can
assist an explicit named apply but are never exact or auto-apply eligible. Raw
physical fields must corroborate an exact ID so lossy ID slug collisions cannot
be promoted to exact matches. An exact description is the minimum high-confidence physical
match. Every profile monitor and current monitor must participate for an exact
or high-confidence automatic match. Output-name-only and incomplete matches are
never auto-applied. Any equally good monitor or profile mapping is reported as
ambiguous and blocks fallback application. `fallback_profile` is considered
only when the result is unambiguous and no eligible match exists.

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
cargo run -- apply desk
cargo run -- apply desk --unattended
cargo run -- apply --auto --dry-run
cargo run -- daemon --once --dry-run
cargo run -- daemon --unattended
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
match is found. It is not used to bypass an ambiguous match. Unknown config
keys are rejected so spelling mistakes do not silently change behavior.

The daemon waits `debounce_ms` after monitor hot-plug events before querying
Hyprland again. Values are limited to `0..=60000`; zero disables debounce.
`apply_on_start` is disabled by default so starting the daemon
does not immediately change monitor layout. Event bursts reset the debounce
deadline. Unrelated and malformed socket2 lines are ignored. Exact or eligible
high-confidence matches are applied; ambiguous, missing, and ineligible matches
are logged without changing outputs. Already-active plans are skipped, and one
immediate identical retry after a failed or rejected transaction is suppressed
to break self-generated event loops. Socket disconnects and connection failures
are logged, retried after two seconds, and reconciled after reconnect. If an
inherited `HYPRLAND_INSTANCE_SIGNATURE` becomes stale, a uniquely discoverable
live socket replaces it for socket2 and `hyprctl` operations.
Socket discovery requires a connectable, user-owned instance; stale socket
inodes, unsafe signature components, symlinked instance directories, oversized
events, and invalid UTF-8 frames are rejected.

`apply_on_start` does not bypass safety. A foreground daemon can ask for the
15-second confirmation, but a service normally has no terminal and therefore
must be started with `--unattended` to change layouts. This opt-in is a CLI
policy rather than a persistent config value, so it is visible in the process
and service command line.

The TUI moves the selected monitor by `tui_move_step` logical pixels when using
the arrow-key nudge controls. The TUI shows this configured step in its action
area. Valid values are `1` through `10000`.

## TUI Keys

- `[` / `]`: previous or next saved profile; `Tab` / `Shift-Tab`: monitor
- Arrow keys: nudge the selected monitor; `H` / `J` / `K` / `L`: snap it
- `Space`: enable/disable; `m`: choose an advertised mode; `r`: transform 0–7
- `+` / `-`: scale by 0.1; mouse selection and dragging are also supported
- `n`: new draft; `s`: save as; `c`: copy; `R`: rename; `d`: delete
- `a`: apply the shared plan; `A`: run automatic selection and show its reason
- `f`: refresh Hyprland state; `?`: help; `q` or `Esc`: quit/cancel

Replacing, deleting, applying a warned layout, and quitting with unsaved edits
require confirmation. Every changed TUI apply additionally uses the 15-second
post-apply `y` confirmation and rollback. Profile navigation cannot silently
discard a modified draft. Validation and apply errors remain visible inside the
editor.

## Daemon Install

Install the foreground daemon as a systemd user service:

```text
hyprdisjust install-systemd-user --unattended --enable --start
```

This writes `~/.config/systemd/user/hyprdisjust.service` with an `ExecStart`
pointing at the current `hyprdisjust` executable. Use `--dry-run` to inspect the
service file without writing or calling `systemctl`. A real installation runs
`systemctl --user daemon-reload` before optional enable/start operations.
`--unattended` is copied into the service's daemon command and is required for
automatic layout changes because systemd services do not have an interactive
terminal. Omitting it is conservative: matching decisions are still logged,
but a changed layout is refused before apply.

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
  command and states when the live operation would be a no-op. Live apply uses Hyprland's `eval`
  `hl.monitor(...)` form so it works with the newer non-legacy config parser.
- If an apply fails or does not converge, HyprDisJust attempts to restore the
  captured previous monitor layout and verifies the rollback. If rollback also
  fails, the error reports both failures and the printed previous layout can be
  used for manual recovery.
- Timeout, rejected confirmation, or EOF is a safe cancellation when rollback
  succeeds. A confirmation read failure returns an error after restoration.
- `--unattended` skips only the post-apply confirmation. It does not skip plan
  validation, state convergence checks, or recovery after a partial apply.
- `hyprdisjust doctor` reports stale output-name hints when a dock or adapter
  renamed outputs but physical identities still match.
- Generated Lua files live under `~/.config/hyprdisjust/generated/`;
  HyprDisJust does not edit `hyprland.conf`.
- Delete and overwrite operations require explicit confirmation flags or TUI
  confirmation.
- If identical serial-less monitors cannot be distinguished after every
  connector name changes, automatic apply refuses the ambiguous mapping. Give
  those layouts explicit named-apply review instead of weakening daemon safety.

## Shell Completions

Example:

```text
hyprdisjust completions bash > ~/.local/share/bash-completion/completions/hyprdisjust
hyprdisjust completions zsh > ~/.zfunc/_hyprdisjust
hyprdisjust completions fish > ~/.config/fish/completions/hyprdisjust.fish
```

`elvish` and `powershell` are also accepted completion targets.

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
requirements are Hyprland 0.55.1 or newer, `hyprctl`, and a user session where
`XDG_RUNTIME_DIR` is available. `HYPRLAND_INSTANCE_SIGNATURE` is recommended.
When the instance signature is absent but exactly one socket2 instance exists,
HyprDisJust discovers it for both the daemon and `hyprctl`. systemd is optional
and needed only for user-service installation.

Profile/config inputs and subprocess output are size-bounded. Profile CRUD is
serialized through `profiles.lock`, reloads under the lock, validates the full
candidate store, and atomically replaces private `0600` data. Generated files
and systemd units use the same symlink-safe atomic replacement path. External
commands have operation-specific deadlines and capped stdout/stderr retention.
Optional daemon file logging rotates continuously at 1 MiB and retains one
archive; systemd deployments can omit `--log-file` and use journald.

## MVP Limitations

- Export currently targets Hyprland's Lua `hl.monitor(...)` configuration path;
  classic `monitors.conf` export is intentionally not exposed.
- Identical monitors without useful serial/EDID distinctions remain ambiguous
  if connector names also change.
- Workspace migration, HDR/color controls, hooks, and other compositor policy
  are outside the monitor-profile MVP.

## License

HyprDisJust is distributed under the [MIT License](LICENSE).

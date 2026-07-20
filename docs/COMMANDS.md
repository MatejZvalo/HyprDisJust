# HyprDisJust Command Reference

## Diagnostics

- `hyprdisjust doctor`
  Prints config paths, generated config paths, Hyprland session status, socket2
  availability, profile count, monitor identities, stale output-name hints,
  systemd guidance, and the best automatic profile decision with score and
  refusal reason.
  Returns a nonzero status if any diagnostic has error severity.

## Profiles

- `hyprdisjust save [name] [--replace]`
  Saves the current monitor layout. Explicit names are never overwritten unless
  `--replace` is passed.
- `hyprdisjust list`
  Lists saved profiles.
- `hyprdisjust rename <old> <new>`
  Renames an existing profile. The new name must not already exist.
- `hyprdisjust copy <source> <dest> [--replace]`
  Duplicates a profile under a new name.
- `hyprdisjust delete <name> [--yes]`
  Deletes a profile. Non-interactive use requires `--yes`.

## Apply And Export

- `hyprdisjust apply <name> [--dry-run] [--unattended]`
  Applies a named profile or prints the generated operation and command. A live
  no-op is detected and skipped. Changed live applies require `y` within 15
  seconds or the captured previous layout is restored. `--unattended` is the
  explicit automation bypass.
- `hyprdisjust apply --auto [--dry-run] [--unattended]`
  Selects the best exact or high-confidence profile, using `fallback_profile`
  only when no eligible unambiguous match exists. Ambiguity always refuses.
- `hyprdisjust export [name] --format lua`
  Writes `generated/monitors.lua`.

## Daemon

- `hyprdisjust daemon [--unattended] [--log-file PATH]`
  Watches Hyprland socket2 monitor events and auto-applies eligible profiles.
  Monitor-event bursts are debounced, unrelated events are ignored, malformed
  lines do not trigger applies, disconnects reconnect, and active layouts are
  not reapplied. Reconnects immediately reconcile current topology, and one
  identical retry after a failed or rejected transaction is suppressed to break
  self-generated event loops. Without `--unattended`, every changed apply
  requires the same 15-second interactive confirmation and non-terminal
  sessions refuse before changing outputs.
  `--log-file` appends sanitized `0600` logs to a file in a private directory.
  The active file rotates at 1 MiB to a single `.log.1` archive, bounding retained
  file logs to roughly 2 MiB; without this option output is written only to
  stdout/stderr (and therefore normally captured by journald under systemd).
- `hyprdisjust daemon --once --dry-run`
  Runs one automatic decision and prints the generated command.
- `hyprdisjust install-systemd-user [--enable] [--start] [--dry-run] [--unattended]`
  Writes `~/.config/systemd/user/hyprdisjust.service`, reloads the user manager,
  and optionally enables or starts the daemon. Dry-run performs none of these
  changes. `--unattended` deliberately adds that flag to the installed daemon
  command; without it a non-interactive service will not change layouts.

## TUI

- `hyprdisjust tui`
  Opens the terminal profile editor. It can create, save, copy, rename, delete,
  edit, preview, auto-select, refresh, and apply profiles. Plans with warnings
  require confirmation before apply.

TUI keys:

- `[` / `]` profiles; `Tab` / `Shift-Tab` monitors
- arrows nudge; `H` / `J` / `K` / `L` snap; mouse drag moves
- `Space` enable; `m` mode; `r` transform; `+` / `-` scale
- `n` new; `s` save-as; `c` copy; `R` rename; `d` delete
- `a` apply; `A` automatic selection; `f` refresh; `?` help; `q` quit

The TUI keeps terminal I/O separate from draft state. Recoverable validation,
persistence, refresh, and apply errors are shown in the editor, and terminal
state is restored when the editor exits. Changed TUI applies use the same
15-second post-apply confirmation and rollback transaction as the CLI.

## Matching And Validation

Matching is physical-identity first and globally one-to-one. Exact IDs outrank
exact descriptions; make/model and output-name-only matches remain partial.
Connector-only and connector-disambiguated IDs never qualify as exact or for
automatic apply, and raw physical fields must corroborate exact IDs.
Enabled and disabled outputs require a concrete `WIDTHxHEIGHT@REFRESH` mode or
`preferred`, `highres`, `highrr`, or `maxwidth`; scale must be in `0.1..=10.0`
and produce whole logical pixels. Transform must be `0..=7`, monitor IDs must be
unique, and output mappings must be consistent. An apply
that would disable every saved output is refused. Active outputs are configured
before obsolete outputs are disabled, then the resulting state is verified; a
failure triggers a captured-layout rollback attempt. Rollback is also verified
against the exact normalized state captured before apply. Previously disabled
outputs have their captured mode, position, scale, and transform restored before
their disabled state is reinstated.
All live applies are serialized from the authoritative monitor query through
confirmation or rollback. Verification checks the complete output set and
identity, and allows bounded backoff for delayed modesets.

## Shells

- `hyprdisjust completions bash`
- `hyprdisjust completions elvish`
- `hyprdisjust completions zsh`
- `hyprdisjust completions fish`
- `hyprdisjust completions powershell`

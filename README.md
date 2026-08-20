# herdr-tilt

A keyboard-driven Tilt dashboard for Herdr. It opens beside the current pane,
shows every Tilt service with a four-color status circle, and starts or stops
Tilt without tying the Tilt process to the dashboard pane.

## Requirements

- Herdr 0.7.0 or newer
- Tilt on `PATH`
- macOS or Linux

Installing from source also requires a current stable Rust toolchain.

## Install for development

Build the binary and link this checkout:

```sh
cargo build --release --locked
herdr plugin link .
```

After changing `herdr-plugin.toml`, relink it so Herdr revalidates the manifest:

```sh
herdr plugin unlink herdr.tilt
herdr plugin link .
```

## Configure the opening key

Herdr deliberately leaves plugin keys to the user. Add a binding to
`~/.config/herdr/config.toml`:

```toml
[[keys.command]]
key = "prefix+t"
type = "plugin_action"
command = "herdr.tilt.open"
description = "open Tilt status"
```

Apply the change with:

```sh
herdr server reload-config
```

The action is idempotent: it focuses the existing dashboard for the project or
opens a right-hand split when no dashboard exists.

## Dashboard keys

| Key | Action |
| --- | --- |
| `↑`/`↓` or `k`/`j` | Move through services |
| `Page Up`/`Page Down` | Move ten services at a time |
| `Home`/`End` | Jump to the first or last service |
| `Enter` or `Space` | Toggle the selected resource group |
| `t` | Trigger a rebuild of the selected service |
| `e` | Enable or disable the selected service |
| `u` | Start a retained `tilt up` session |
| `d`, `d` | Confirm, stop Tilt, and run `tilt down` |
| `r` | Refresh immediately |
| `q` or `Esc` | Close the dashboard without stopping Tilt |

The dashboard opens even when the workspace has no `Tiltfile`, but displays a
warning and disables Up and Down.

## Status colors

- Green: healthy
- Orange: building, queued, pending, or warning
- Red: update, build, or runtime error
- Grey: disabled, inactive, stopped, or unknown

Services are grouped by their Tilt labels. Groups are alphabetical, services
retain Tilt's resource order within each group, multi-labeled services appear
in every applicable group, and unlabeled services appear under `Ungrouped` at
the end. Group headers show their aggregate status and service count and remain
collapsed across dashboard refreshes. The synthetic `(Tiltfile)` resource is
hidden from the service list, while its errors appear in the warning banner.

## Session behavior

Each canonical Tiltfile gets its own lock, state record, API port, and log. The
retained runner is a Herdr plugin action, so closing the dashboard does not stop
Tilt. Reopening the dashboard reads the record and reconnects to the same API.

Before showing services, the dashboard verifies the API-reported Tiltfile and
PID against its state record. Before signaling a PID on Down, it verifies that
the per-project lock is still held, preventing stale state from targeting a
reused process ID.

Plugin runtime data is stored under the `HERDR_PLUGIN_STATE_DIR` that Herdr
injects into plugin processes. Find the user-editable config directory and
inspect action logs with:

```sh
herdr plugin config-dir herdr.tilt
herdr plugin log list --plugin herdr.tilt
```

## Current scope

- Plugin-started Tilt sessions are managed automatically.
- A manually started Tilt on the default port (`10350`) is discovered after
  its API-reported Tiltfile is verified against the current workspace.
- A manually started Tilt on an arbitrary non-default port is not
  auto-discovered.
- Tiltfile arguments and custom `tilt down` flags are not yet configurable.
- Windows is not yet supported because retained process signaling needs a
  platform-specific implementation.

## Development checks

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo build --release --locked
```

See [the research note](docs/research/herdr-tilt-plugin.md) for the architecture
and primary-source references behind the implementation.

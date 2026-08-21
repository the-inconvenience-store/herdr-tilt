# herdr-tilt

A keyboard-driven Tilt dashboard for Herdr. It opens beside the current pane,
shows every Tilt service with a four-color status circle, and starts or stops
Tilt without tying the Tilt process to the dashboard pane.

## Requirements

- Herdr 0.7.0 or newer
- Tilt on `PATH`
- macOS or Linux

No Rust toolchain is needed for a normal installation. The installer downloads
and verifies a prebuilt binary for Apple Silicon macOS, Intel macOS, ARM64
Linux, or x86-64 Linux.

## Install

Install a tagged release directly from GitHub:

```sh
herdr plugin install the-inconvenience-store/herdr-tilt --ref v0.1.3
```

Herdr clones the tagged source, then the plugin installer downloads the
matching release binary and verifies its SHA-256 checksum. To upgrade, run the
same command with the newer release tag. If this checkout is currently linked
for development, unlink it before installing the managed copy:

```sh
herdr plugin unlink herdr.tilt
```

## Install for development

Development requires a current stable Rust toolchain. Build the binary before
linking because `plugin link` intentionally does not run manifest build steps:

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

Add a binding to `~/.config/herdr/config.toml`:

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
| `l` | Open live logs for the selected service |
| `a` | Open or run actions for the selected service |
| `w` | Open the active Tilt Web UI in the system browser |
| `u` | Start a retained `tilt up` session |
| `d`, then `y` | Confirm, stop Tilt, and run `tilt down` |
| `n` | Cancel a pending Down confirmation |
| `r` | Refresh immediately |
| `?` | Show or hide all dashboard keybinds |
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

Services with Tilt endpoint links or custom resource-scoped UI buttons show a
right-aligned `↗` marker (and an action count when there is more than one).
Selecting the service expands that marker into an inline preview of its action
titles.

Press `a` to run the only action immediately or open a picker when multiple
actions are available. Use `↑`/`↓` or `j`/`k` in the picker, `Enter` or `Space`
to activate, and `q` or `Esc` to return. URL actions open in the system browser.
Buttons that Tilt marks as requiring confirmation prompt for `y` or `n`, and
button inputs use their Tilt-configured defaults. Tilt's built-in Stop Build
and Disable Toggle buttons are omitted because the dashboard already provides
dedicated trigger and enable/disable controls.

## Live logs

Press `l` on a service to replace the dashboard with its live logs. For
each selected service, the viewer runs separate Tilt `build` and `runtime`
streams. Build/Tilt output is prefixed with `tilt │`, while the service's
application stdout/stderr is prefixed with `app │`. Because both streams come
through Tilt, application logs continue following across pod replacements and
also work for local resources.
The viewer removes terminal escape sequences, expands JSON records, highlights
warnings and errors, and remains anchored to the newest visual line while
following.

| Key | Action |
| --- | --- |
| `↑`/`↓` or `k`/`j` | Scroll one log record |
| `Page Up`/`Page Down` | Scroll one viewport |
| `Home`/`g` or `End`/`G` | Jump to the oldest record or resume at the tail |
| `f` | Toggle follow mode |
| `w` | Toggle line wrapping |
| `←`/`→` or `h`/`l` | Scroll horizontally when wrapping is disabled |
| `c` | Clear the current scrollback |
| `q` or `Esc` | Return to the dashboard |

For predictable memory use, scrollback is capped at 2,000 records, each record
is capped at 8 KiB while it is read, and the reader-to-UI channel holds at most
128 pending records. Closing the viewer terminates and reaps both `tilt logs`
processes while leaving Tilt and the workloads themselves running.

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

Release maintainers should follow [the release guide](docs/releasing.md). A
tagged release builds and publishes all four supported binaries and their
checksums; do not upload release assets by hand.

See [the research note](docs/research/herdr-tilt-plugin.md) for the architecture
and primary-source references behind the implementation.

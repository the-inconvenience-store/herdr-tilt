# Herdr Tilt plugin research

Research date: 2026-08-20

## Executive summary

This plugin is feasible with Herdr's v1 plugin API, but it should not run `tilt
up` inside the visible status pane. Herdr plugin panes are real terminal panes,
and the pane process ends when the pane is closed; `tilt up` is itself the
foreground, long-running owner of Tilt's engine and API server. The robust
shape is therefore:

1. a user-configured Herdr key invokes a manifest action;
2. that action resolves the active workspace directory, then focuses an
   existing status pane or opens a right/down split (the pane itself shows a
   warning and disables Up when there is no `Tiltfile`);
3. the pane runs only the TUI client;
4. a separate plugin-owned controller starts and retains one `tilt up` process
   per workspace, plus its PID, port, exact Tiltfile path, and logs under
   `HERDR_PLUGIN_STATE_DIR`; and
5. reopening the panel reconnects to that controller/Tilt API instead of
   starting a second Tilt instance.

The first implementation should read status through the supported Tilt CLI
(`tilt get uiresources -o json`) behind an adapter. Direct API watches can be a
later optimization. `UIResource` is explicitly described as legacy transition
data, so parsing needs to tolerate absent fields and unknown enum values.

## How Herdr plugins work

### Package and execution model

A Herdr plugin is a directory containing `herdr-plugin.toml` plus any scripts
or binaries it launches. It is not loaded into Herdr's process and there is no
language SDK: the manifest declares executable actions, event hooks, startup
hooks, panes, and link handlers, while plugin programs call the Herdr CLI or
socket API. Manifest `command` fields are argv arrays and do not receive shell
expansion unless they explicitly invoke a shell. Runtime registration and
native non-terminal plugin UI are not available in v1. [Herdr plugin authoring
reference](https://herdr.dev/docs/plugins/)

The required manifest metadata is `id`, `name`, `version`, and
`min_herdr_version`. Platform declarations can exist globally and per
entrypoint. Herdr can install a public GitHub plugin with `herdr plugin install
owner/repo[/subdir]`, or link a local checkout with `herdr plugin link PATH`;
linking deliberately does not execute build commands. [Herdr plugin authoring
reference](https://herdr.dev/docs/plugins/)

Plugins are unsandboxed code running as the user, with their inherited
environment and access to the full Herdr CLI. Installation presents source and
command previews, but trust remains the user's responsibility. [Herdr plugin
security model](https://herdr.dev/docs/plugins/#trust-and-security)

### Actions, context, and user keybindings

The manifest should expose a workspace action such as `open`:

```toml
[[actions]]
id = "open"
title = "Open Tilt status"
contexts = ["workspace"]
command = ["bin/herdr-tilt", "open"]
```

Herdr plugins cannot assign their own default keys. Users bind an installed
action in `~/.config/herdr/config.toml`, which is exactly the requested
customizable behavior:

```toml
[[keys.command]]
key = "prefix+t"
type = "plugin_action"
command = "herdr.tilt.open"
description = "open Tilt status"
```

Herdr documents this `plugin_action` binding and its qualified action ID.
[Herdr plugin keybindings](https://herdr.dev/docs/plugins/#keybindings)

Runtime commands start in the plugin directory, not the project directory.
Herdr injects `HERDR_BIN_PATH`, `HERDR_PLUGIN_ROOT`, config/state directories,
workspace/tab/pane IDs, and `HERDR_PLUGIN_CONTEXT_JSON`. The context schema
includes `workspace_cwd`, `focused_pane_cwd`, workspace ID, focused pane ID,
and worktree provenance. In the current host implementation, `workspace_cwd`
can itself resolve from the focused pane, so it is not always a fixed workspace
root. The plugin should prefer the focused pane directory, use a worktree
checkout path (when present) as the upward-search boundary, and otherwise fall
back to the supplied workspace directory without walking outside it. [Herdr runtime
environment](https://herdr.dev/docs/plugins/#commands-and-environment), [exact
context schema in Herdr source](https://github.com/herdrdev/herdr/blob/ffc4e263168f9e81d5bbc14db4b16ca9818d684a/src/api/schema/plugins.rs#L364-L391)

### Pane behavior and reuse

A manifest `[[panes]]` entrypoint supplies the TUI command. The action opens it
with:

```text
herdr plugin pane open --plugin herdr.tilt --entrypoint status \
  --placement split --direction right --cwd PROJECT --target-pane PANE --focus
```

`plugin pane open` supports `overlay`, `popup`, `split`, `tab`, and `zoomed`.
Split, tab, zoomed, and overlay results are normal Herdr panes and preserve
plugin ownership if moved. Popups are session-modal singletons and do not
participate in pane/layout/persistence APIs, so a split is the better fit for
the requested persistent side panel. A split needs a target pane; `--cwd`
explicitly sets the new pane's project directory. [Herdr pane semantics](https://herdr.dev/docs/plugins/#panes),
[Herdr CLI pane arguments](https://herdr.dev/docs/cli-reference/#plugins)

The action should persist the returned pane ID, attempt `herdr plugin pane
focus ID` on later invocations, and open a replacement when focus reports that
the saved pane no longer exists. The Neon Herdr plugin is a representative
working example of precisely this idempotent open-or-focus pattern: it stores
the pane ID, tries to focus it, and only calls `plugin pane open` when that
fails. [Neon open-dashboard command](https://github.com/neon-solutions/neon-herdr/blob/c64120977a5c9fe4c44f703509be70ca796a6db6/src/commands/open-dashboard.ts),
[Neon Herdr adapter](https://github.com/neon-solutions/neon-herdr/blob/c64120977a5c9fe4c44f703509be70ca796a6db6/src/infrastructure/herdr/herdr-live.ts)

Other useful representative examples are:

- Plugin Manager: a small Bash TUI declared as a popup plus an `open` action,
  with the user adding the keybinding. [manifest](https://github.com/speardragon/herdr-plugin-manager/blob/3b1f6e1be4ad811e39e4fecb24cd3a976b692241/herdr-plugin.toml),
  [README](https://github.com/speardragon/herdr-plugin-manager/blob/3b1f6e1be4ad811e39e4fecb24cd3a976b692241/README.md)
- herdr-reviewr: a Rust TUI with manifest `toggle`, `open`, and `close`
  actions around a split pane. [manifest](https://github.com/persiyanov/herdr-reviewr/blob/e7d88534588f8865b9e14cc65f353596aa571427/herdr-plugin.toml),
  [pane controller](https://github.com/persiyanov/herdr-reviewr/blob/e7d88534588f8865b9e14cc65f353596aa571427/herdr/pane.sh)
- Herdr's example repository: small examples for build commands, startup/event
  hooks, actions, and panes. Herdr's own docs call these a cookbook rather than
  maintained official plugins. [example repository](https://github.com/ogulcancelik/herdr-plugin-examples/tree/18709cdc851dd63ed0543eb8388343a5446fd8d8)

Herdr startup hooks are one-shot initialization commands, not supervised
daemons. They run once after server restore and API readiness, and again on
live handoff. This means process retention must be implemented by the plugin's
controller rather than by assuming a startup hook will supervise Tilt. [Herdr
startup hooks](https://herdr.dev/docs/plugins/#startup-hooks)

## Tilt behavior relevant to the plugin

### `tilt up`, Tiltfile detection, and `tilt down`

`tilt up` starts the services defined by the Tiltfile and remains in the
foreground. With no `-f/--file`, Tilt looks for a file named exactly `Tiltfile`
in the current directory. There is no documented recursive parent search or
detached mode, so the plugin should check `PROJECT/Tiltfile` itself and show a
Herdr notification/TUI warning instead of launching Tilt's interactive starter
Tiltfile prompt. [Tilt tutorial](https://docs.tilt.dev/tutorial/2-tilt-up.html),
[`tilt up` reference](https://docs.tilt.dev/cli/tilt_up.html), [Tilt's
non-interactive/missing-Tiltfile handling](https://github.com/tilt-dev/tilt/blob/0e8dae38b5fb0fc8ba2ceb9a38aa65be528220ee/internal/cli/generate_tiltfile.go#L29-L56)

When `tilt up` exits, Kubernetes and Docker Compose resources remain deployed,
but long-running local resources are terminated. `tilt down` is a separate
command that reevaluates the Tiltfile and deletes its resources. It does not
delete namespaces or Docker volumes by default, and Kubernetes objects marked
with `tilt.dev/down-policy: keep` remain. [`tilt up` lifecycle](https://docs.tilt.dev/cli/tilt_up.html),
[`tilt down` reference](https://docs.tilt.dev/cli/tilt_down.html), [Tilt down
implementation](https://github.com/tilt-dev/tilt/blob/0e8dae38b5fb0fc8ba2ceb9a38aa65be528220ee/internal/cli/down.go)

Consequently, the TUI's Down key should mean two explicit operations:

1. gracefully signal and wait for the plugin-owned `tilt up` process; then
2. run `tilt down -f EXACT_TILTFILE_PATH` with the same project environment.

Running only `tilt down` cleans deployed resources but does not define the
lifecycle of the still-running `tilt up` process.

### API and CLI access

Tilt exposes a Kubernetes-style API server. The supported CLI can enumerate
objects with `tilt api-resources`, inspect schemas with `tilt explain`, and
return machine-readable objects with `tilt get ... -o ...`. `tilt get` supports
watch mode as well as `--host` and `--port`. [Tilt API overview](https://api.tilt.dev/),
[`tilt get` reference](https://docs.tilt.dev/cli/tilt_get.html)

For an MVP, poll:

```text
tilt get uiresources -o json --host localhost --port PORT
tilt get session -o json --host localhost --port PORT
```

Polling through the installed Tilt binary avoids duplicating the API server's
client authentication/config behavior. A later version can use `tilt get
uiresources -o json --watch` or the API list/watch endpoints, with reconnect
and full-resync logic. The official OpenAPI spec exposes the list endpoint at
`/apis/tilt.dev/v1alpha1/uiresources`. [Tilt OpenAPI
specification](https://github.com/tilt-dev/api.tilt.dev/blob/main/openapi-spec/swagger.json)

The default server address is `localhost:10350`; `TILT_PORT` or `--port`
selects another port. Since one Herdr session can contain several workspaces,
the plugin cannot safely assume all projects use 10350. It should allocate and
persist a stable loopback port per project and pass it to every `tilt up` and
`tilt get` call. [Tilt connection flags](https://github.com/tilt-dev/tilt/blob/0e8dae38b5fb0fc8ba2ceb9a38aa65be528220ee/internal/cli/flags.go#L24-L74),
[default port constant](https://github.com/tilt-dev/tilt/blob/0e8dae38b5fb0fc8ba2ceb9a38aa65be528220ee/pkg/model/web.go#L85)

### Resource and session data

`UIResource` is the object designed for per-resource UI status. Its important
fields for the TUI are `metadata.name`, `status.order`, `updateStatus`,
`runtimeStatus`, `disableStatus.state`, `currentBuild`, `queued`,
`buildHistory`, `conditions`, endpoint links, and runtime-specific details.
The API reference explicitly warns that this is a legacy transition structure,
so it should be isolated behind an internal adapter. [UIResource API](https://api.tilt.dev/interface/ui-resource-v1alpha1.html)

Exact current enums are:

- update: `none`, `in_progress`, `ok`, `pending`, `error`, `not_applicable`;
- runtime: `unknown`, `ok`, `pending`, `error`, `not_applicable`, `none`.

[Update status source](https://github.com/tilt-dev/tilt/blob/0e8dae38b5fb0fc8ba2ceb9a38aa65be528220ee/pkg/apis/core/v1alpha1/updatestatus_types.go),
[runtime status source](https://github.com/tilt-dev/tilt/blob/0e8dae38b5fb0fc8ba2ceb9a38aa65be528220ee/pkg/apis/core/v1alpha1/runtimestatus_types.go)

The synthetic `(Tiltfile)` UIResource should not appear as an application
service row, but its load/update error should be surfaced prominently. Tilt's
official resource documentation and CLI tree view both treat `(Tiltfile)` as
the root resource. [Tilt resource dependencies](https://docs.tilt.dev/resource_dependencies.html),
[Tilt tree-view source](https://github.com/tilt-dev/tilt/blob/0e8dae38b5fb0fc8ba2ceb9a38aa65be528220ee/internal/cli/tree_view.go#L30-L30)

`Session` provides an exact `spec.tiltfilePath` plus server PID/start time;
`UISession` provides `fatalError`, `tiltStartTime`, `tiltfileKey`, and Tilt
version. These are the right guards against stale PID files, PID reuse, port
collisions, or accidentally connecting one workspace's panel to another
workspace's Tilt server. [Session API](https://api.tilt.dev/core/session-v1alpha1.html),
[UISession API](https://api.tilt.dev/interface/ui-session-v1alpha1.html)

### Four-color status mapping

Tilt has two status axes rather than one. Its current CLI gives errors priority,
then building/pending, then unknown/not-started, and finally OK; Tilt's Web UI
has additional warning information from its log alert index. [Tilt CLI status
precedence](https://github.com/tilt-dev/tilt/blob/0e8dae38b5fb0fc8ba2ceb9a38aa65be528220ee/internal/cli/tree_view.go#L551-L596),
[Tilt Web UI status logic](https://github.com/tilt-dev/tilt/blob/0e8dae38b5fb0fc8ba2ceb9a38aa65be528220ee/web/src/status.tsx)

Recommended mapping for the requested circles:

| Circle | Meaning | Rule |
| --- | --- | --- |
| red | unhealthy | update or runtime is `error` |
| orange | working / attention | update is `in_progress` or `pending`, runtime is `pending`, queued/current build, or latest build has warnings |
| green | healthy | applicable update and runtime axes are `ok` (treat `not_applicable` as neutral) |
| grey | inactive / unknown | disabled, `none`, `unknown`, no server, malformed/new status, or not started |

This is intentionally a product mapping, not an assertion that Tilt defines
these four colors. In particular, an API-only client cannot reproduce Tilt's
warning state exactly because the Web UI derives alerts from logs; using the
latest `buildHistory[].warnings` is only an approximation.

## Proposed component boundaries

```text
Herdr plugin action (open/toggle)
  -> resolve workspace_cwd + Tiltfile check
  -> validate/focus saved panel pane, otherwise plugin pane open

Status TUI pane
  -> reads controller state for project
  -> polls Tilt CLI/API adapter
  -> renders ordered services and four-state circles
  -> Up/Down keys call controller commands
  -> exits without stopping Tilt

Project controller
  -> exclusive lock per canonical Tiltfile path
  -> allocates/reuses a loopback port
  -> starts foreground `tilt up --stream --port PORT -f PATH` as a retained child
  -> records PID/start identity/port/log path atomically
  -> validates identity against Tilt Session before reuse or signal
  -> on Down: graceful stop, wait, `tilt down -f PATH`, clear state
```

Recommended state key: a collision-resistant digest of the canonical absolute
Tiltfile path, not merely the workspace label or basename. Store the canonical
path in the record as a human-auditable identity. Use an exclusive lock so
rapid repeated key presses cannot start duplicate engines.

The controller, not the pane, owns Tilt. This directly satisfies the
requirement that closing and reopening the panel reuses the running Tilt
session. Persistence across a Herdr *client detach* naturally follows because
the Herdr server and its children remain running; persistence across `herdr
server stop`, machine reboot, or plugin upgrade is a separate recovery feature
and should be specified explicitly before implementation.

## Risks and decisions to settle before implementation

1. **Platform scope.** A portable retained-process controller and signal model
   differs between Unix and Windows. The initial plugin can reasonably declare
   `platforms = ["macos", "linux"]` unless Windows is a launch requirement.
2. **External Tilt instances.** Tilt documents host/port configuration but no
   project-wide discovery protocol. MVP scope should either manage only
   plugin-started Tilt instances or expose an optional configured host/port.
3. **Pane placement.** Choose right split or down split as the default action
   behavior, while keeping the user's key configurable. The manifest itself
   cannot make direction user-configurable; plugin-owned config can.
4. **Down semantics.** Confirm that the TUI should both stop Tilt and clean
   deployed resources. This note assumes yes because the requested operation
   is called “Tilt down.”
5. **Failure visibility.** Missing Tilt binary, missing Tiltfile, occupied port,
   controller crash, Tilt fatal error, and down failure should appear in the
   TUI and as a concise Herdr notification when invoked from a closed panel.
6. **API compatibility.** Unknown statuses must map to grey, and the adapter
   needs fixture tests from multiple supported Tilt versions because
   UIResource/UISession are explicitly legacy.

## Primary sources consulted

- [Herdr plugin documentation](https://herdr.dev/docs/plugins/)
- [Herdr configuration documentation](https://herdr.dev/docs/configuration/)
- [Herdr CLI reference](https://herdr.dev/docs/cli-reference/)
- [Herdr plugin host source at researched commit](https://github.com/herdrdev/herdr/tree/ffc4e263168f9e81d5bbc14db4b16ca9818d684a/src/app/api/plugins)
- [Tilt API reference](https://api.tilt.dev/)
- [Tilt CLI reference](https://docs.tilt.dev/cli/tilt_up.html)
- [Tilt source at researched commit (0.37.7)](https://github.com/tilt-dev/tilt/tree/0e8dae38b5fb0fc8ba2ceb9a38aa65be528220ee)

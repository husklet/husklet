# @husklet/mcp

An MCP server for an LLM agent using a capability-scoped Husklet extension
socket. It receives exactly the capabilities granted to that socket credential.

```sh
npx -y @husklet/mcp --socket /path/to/extension.sock --workspace dev
```

Common stdio MCP clients accept this copy-paste configuration shape:

```json
{
  "mcpServers": {
    "husklet": {
      "command": "npx",
      "args": ["-y", "@husklet/mcp", "--socket", "/path/to/extension.sock", "--workspace", "dev"]
    }
  }
}
```

The socket path is a credential, not a discovery endpoint: install an observer
extension with only the grants the client needs and use its host-provisioned
socket. Both arguments are mandatory, duplicates and unknown flags are refused,
and startup verifies `workspace_info.name` before exposing MCP tools. Diagnostics
go to stderr so they cannot corrupt the JSON-RPC stream on stdout. This command
does not install extensions, discover credentials, or modify client settings.
Startup abandons a socket that does not complete its host greeting within five
seconds. Host EOF or socket removal terminates the MCP process with an actionable
stderr diagnostic; client EOF, SIGINT, and SIGTERM close both transports cleanly.
The CLI never reconnects automatically: a replacement socket may carry different
authority and must be selected and workspace-verified explicitly by the client.

Tools use strict schemas and bounded, redacted results. Pane snapshots are
deterministic XML-like text carrying stable revisions, node IDs, roles, state,
and actions; other tools use JSON. Container execution state is available as a
read-only inspection tool under the host's `ContainerRead` grant. Container exec
accepts only a bounded argv vector under `ContainerControl`. The
`husklet_terminal_spawn` tool replaces one discovered terminal pane's process
under `TerminalControl` with an exact 1..=64 element argv vector (4096 bytes per
argument, 32768 bytes total). It never parses shell command text and does not
grant access to a pane the socket authority cannot already control.
Pane semantic tools appear only when the installed
`@husklet/react` exposes the host-backed `terminal.semantics` and `terminal.act`
methods.

`husklet_workspace_event_wait` observes one bounded keyboard, focus, or pointer
event batch under the distinct `WorkspaceEvents` grant. It subscribes with one
unit of host credit, reports the host's dropped/coalesced count, and always
unsubscribes on a match or timeout. It observes window-level input only; owned
surface interactions remain addressed to their owning extension and are queued
without blocking the native UI.

Execution inspection and execution signaling use distinct typed calls: signaling
targets one complete immutable execution ID under `ContainerControl` and accepts
only a 1..=32 byte signal name. Container stop, removal, and signaling likewise
require the complete 32- or 64-hex identity returned by inventory or inspection.
Names, prefixes, and snapshot PIDs are refused before the socket call. Execution
signaling does not signal the owning container or parse a shell.
Execution output replay reports stdout and stderr truncation independently and
sets `eof` only when the process was already complete before replay; an empty
running response is therefore not presented as end-of-stream.
Image lookup and pulls accept tags, while `husklet_image_remove` accepts only a
complete immutable `sha256:` digest plus literal confirmation. A tag cannot be
re-resolved to a different image between inspection and removal.
Network reads may use canonical names. Network removal, connect, and disconnect
instead require the complete 32-hex network ID; attachment changes also require
the complete immutable container ID, preventing name or prefix re-resolution.
Volume removal requires the canonical name, its complete 32-hex observed
generation, and confirmation. The host atomically rejects a generation that no
longer names the current same-name volume.
Container process inspection is a timestamped, bounded snapshot of the initial
process only. Its PID is explicitly snapshot-local and may be reused; the host
does not expose argv or environment values, and does not claim child-process,
CPU, or memory coverage that its current daemon sampler cannot provide.

Container creation accepts bounded entrypoint/argv, environment, working directory,
user, labels, named-volume mounts, one workspace-local network, TCP/UDP exposure,
and memory/CPU/PID limits. Host bind paths are deliberately absent. Published ports
bind `127.0.0.1` only, and creation never pulls an image or starts the container.

Workspace creation and update carry the complete typed configuration: identity,
architecture, storage, resources, environment, mounts, Docker access, terminal,
VPN, scrollback, and execution lifetime. Every collection and string is bounded.
Updates require `confirm: true`, refuse renaming, and still require the host's
`WorkspaceControl` grant and stopped-workspace validation.

Installed extensions can be listed and inspected under `ExtensionRead`.
Enable, disable, and record removal each require literal `confirm: true` and the
host's `ExtensionControl` grant. Acquisition uses the separate
`ExtensionInstall` authority: a confirmed start returns a bounded job, status
reveals the resolved digest, the installed digest observed for that consent
revision, manifest identity, and requested grants, and only a
second confirmed install or update echoes that observed revision and commits the
caller-selected grant; a changed candidate or installed generation therefore
makes stale consent fail. Cancellation is also explicit, confirmed, and bound to
the observed acquisition revision, so it cannot discard a candidate that became
ready after the client last inspected the job; no MCP call performs an
unobservable blocking pull.
`husklet_extension_wait` follows the host's credit-controlled extension topics,
returning either the newest bounded inventory snapshot or acquisition job/revision
metadata. An inventory wait requires the exact `{name, image_digest, status}`
record returned by `husklet_extension_list`; it ignores the subscription's unchanged
initial snapshot and reports status changes, removal, and same-name digest replacement.
The packaged `waitForInstalledExtensionChange` example performs that read-and-arm flow.
A job-specific acquisition wait requires the revision already observed and returns
only a strictly newer revision, so clients can arm before acting without accepting
queued old state. Acquisition notifications never carry manifest contents; clients fetch
status only after an invalidation, and coalescing is reported explicitly.

`husklet_pane_wait` applies the same rule to a specific pane: pass the last
observed generation and revision, and the wait ignores the host's unchanged
initial scan. A replacement generation is newer even when its content revision
starts again at zero.

Pane inventory entries and the outer `husklet_pane_read` XML element carry that
authoritative generation/revision cursor. The packaged day-one workflow reads
the cursor before arming its wait and passes both values back explicitly.

`husklet_container_change_wait` accepts the exact `state` and `created` values
last observed for its immutable container ID. Supplying that cursor prevents the
subscription's unchanged initial catalogue from completing the wait; a state
transition, disappearance, or changed creation identity completes it.

`husklet_execution_change_wait` accepts the complete state and immutable
descriptor returned by execution inspect/catalogue as `after`. It ignores the
subscription's unchanged initial catalogue, reports state transitions, and
returns `replaced: true` if a host ever reuses an execution ID with a different
container, command, or user. Set `absent: true` with that full cursor to wait for
record cleanup; this mode rejects running filters and returns
`{changed:true, execution:null, removed:true}` only when the observed record is
absent. The packaged `waitForExecutionRemoval` helper preserves the full cursor,
and the day-one workflow supplies the same cursor for state changes.

Administrative clients should use `husklet_workspace_mutate_wait` for workspace
create/start/stop/delete workflows. It waits for the host subscription
acknowledgement before invoking control authority, ignores unrelated changes,
and unsubscribes after success, failure, or its bounded timeout. Existing
standalone mutation and wait tools remain available.

Container creation accepts only a bounded image reference and name, while exec
accepts an argv vector rather than shell text. Stopping or signaling the owning
container requires literal `confirm: true`; the signal remains explicit and is
bounded to 32 bytes. Execution-level signaling remains a separate typed tool and
cannot accidentally target the owning container.

Volume and network inventory/inspection use the host's separate `VolumeRead`
and `NetworkRead` grants. Creation and attachment controls retain their
`VolumeWrite` or `NetworkWrite` grants. Volume/network removal and network
disconnect additionally require an explicit `confirm: true` MCP argument.

Image tools list and inspect local images under `ImageRead`. Prefer the bounded
`husklet_image_pull_start` → `husklet_image_pull_wait` →
`husklet_image_pull_status` workflow under `ImageWrite`: it exposes exact job
identity and registry-provided layer/byte progress without polling. Wait is a
filtered one-shot subscription and always releases it; cancel is safe and does
not require destructive confirmation. `husklet_image_pull` remains as a
synchronous compatibility tool. Removing an image or pruning unused images
requires explicit `confirm: true`; the host still enforces `ImageWrite`.

`husklet_pane_list` returns bounded discovery metadata for every inspectable
terminal, extension surface, and native pane, including stable slot and provider
identity without reading contents. It requires the host's `PaneObserve` grant.
Use the returned slot with `husklet_pane_read`, which inspects the split topology and returns one bounded XML
document: terminal panes include screen lines, cursor column/row, focus, grid and tab metadata;
extension surfaces and every inventoried native pane include their semantic tree.
It uses stable slots and semantic IDs, never screenshots, coordinates, or GTK
widget scraping. An inventoried kind without a typed projection, a surface
without semantics, or a terminal absent from topology fails explicitly; it is
never reported as empty text or silently substituted with a screenshot. The older terminal-read and pane-snapshot tools remain for
consumers that need their specific typed result.

Workspace filesystem controls create one directory, rename without overwriting,
or remove one file or empty directory. All paths remain relative to declared
roots; removal requires `confirm: true`, and recursive deletion is unavailable.

Terminal layout tools use the same `beside`/`below` vocabulary as the host and
can focus, split, resize, rebalance, and close panes. Grid sizes and split ratios
are bounded; closing a pane requires an explicit `confirm: true` argument.

`husklet_terminal_write` remains the convenient bounded literal UTF-8 text path.
`husklet_terminal_write_bytes` accepts canonical padded base64 and decodes at
most 65,536 bytes before making any socket call, preserving NUL, control, and
non-UTF8 bytes exactly. Whitespace, URL-safe or unpadded spellings, malformed
padding, and decoded payloads over the protocol limit are rejected.

## Bounded pane-agent turn

[`examples/agent-pane-flow.mjs`](examples/agent-pane-flow.mjs) is an executable
client-side flow for an LLM integration. Pass it an initialized MCP `Client`:

```js
import { runPaneAgentTurn } from '@husklet/mcp/examples/agent-pane-flow.mjs';

const observation = await runPaneAgentTurn(client, {
  actionLabel: 'Refresh',
  terminalBytes: Uint8Array.from([0x03]), // one exact Ctrl-C byte, not shell text
  waitMs: 5_000,
});
```

The turn discovers slots before reading them, reads at most 100 terminal lines,
reads either a native or extension semantic tree, and invokes a node using the
revision returned with that same tree. It writes the supplied bytes through the
canonical-base64 tool. It arms one `husklet_pane_wait` before the action so a
fast change is not lost;
the host's credit-controlled subscription either reports a coalesced change or
times out. Only a reported change causes one fresh snapshot. There is no polling
loop. A stale-revision error is authoritative: read a fresh tree and reconsider
the action instead of replaying an old node ID.

## Day-one control workflow

[`examples/agent-day-one.mjs`](examples/agent-day-one.mjs) composes a complete,
bounded workflow over an initialized MCP `Client`:

```js
import { runAgentDayOne } from '@husklet/mcp/examples/agent-day-one.mjs';

const observation = await runAgentDayOne(client, {
  workspaceName: 'dev-target',
  updatedConfiguration,
  container: {
    image: 'example/worker@sha256:...',
    name: 'agent-check',
    command: ['/usr/bin/worker', '--once'],
  },
  terminalInput: 'status\n',
  actionLabel: 'Refresh',
});
```

It inspects and temporarily updates a target workspace, creates and starts one
container, executes a bounded argv vector, inspects processes, discovers panes,
reads and writes a terminal, reads semantic XML, and invokes a node at the exact
observed revision. One-shot pane waits are armed before terminal input and UI
action. A `finally` block uses confirmed container stop/removal and restores the
original workspace configuration. It never accepts shell command text and does
not retry a stale semantic action.

`husklet_container_attach_terminal` is the interactive counterpart to detached
exec. It accepts only the complete immutable container ID and a bounded argv
array, opens an ephemeral Husklet tab, connects stdin/stdout/stderr through one
TTY, and owns the process with kill-on-disconnect semantics. It requires the
dedicated `container-attach` grant.

`husklet_container_rename` atomically assigns a new name using the complete
32- or 64-hex container ID returned by inventory/create. It rejects aliases and
prefixes; names are 1–128 ASCII bytes, start with an alphanumeric byte, and then
contain only alphanumerics, `_`, `.`, or `-`. Rename is non-destructive and does
not take a confirmation flag. Subsequent container inventory snapshots retain
the immutable ID and carry the new name.

## Administrative lifecycle workflow

[`examples/agent-admin.mjs`](examples/agent-admin.mjs) creates and starts one
named workspace, performs confined directory/file create, write, and read, arms
one pane-change wait, then removes every created resource in reverse order with
literal confirmations. Cleanup also runs after intermediate failure.

Filesystem and pane authority belongs to the socket's hosting workspace; it is
not redirected by a workspace name passed to lifecycle tools. The helper first
calls `husklet_workspace_info`, requires the caller's `hostingWorkspace` to
match, and requires the separately managed workspace to have another name. This
prevents an administrator from assuming that creating `target` makes subsequent
file paths resolve inside `target`. The helper arms `husklet_workspace_wait`
before create, start, stop, and removal and filters by the managed identity and
action. Lifecycle notices carry a monotonic host-process revision and visible
coalescing count under `WorkspaceRead`; an unrelated workspace notice cannot
satisfy the filtered wait. The independent pane wait still demonstrates that
filesystem authority remains attached to the socket workspace.

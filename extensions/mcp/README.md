# @husklet/mcp

An MCP server for an LLM agent running as a Husklet extension. It connects only
to `HUSKLET_EXTENSION_SOCKET` and receives exactly the capabilities granted to
that extension.

```sh
npx @husklet/mcp
```

Tools use strict schemas and bounded, redacted results. Pane snapshots are
deterministic XML-like text carrying stable revisions, node IDs, roles, state,
and actions; other tools use JSON. Container execution state is available as a
read-only inspection tool under the host's `ContainerRead` grant. Container exec
accepts only a bounded argv vector under `ContainerControl`; terminal process
spawning remains absent, so this package provides no unrestricted shell shortcut.
Pane semantic tools appear only when the installed
`@husklet/react` exposes the host-backed `terminal.semantics` and `terminal.act`
methods.

Execution inspection and execution signaling use distinct typed calls: signaling
targets one existing execution ID under `ContainerControl` and accepts only a
1..=32 byte signal name. It does not signal the owning container or parse a shell.

Workspace creation and update carry the complete typed configuration: identity,
architecture, storage, resources, environment, mounts, Docker access, terminal,
VPN, scrollback, and execution lifetime. Every collection and string is bounded.
Updates require `confirm: true`, refuse renaming, and still require the host's
`WorkspaceControl` grant and stopped-workspace validation.

Installed extensions can be listed and inspected under `ExtensionRead`.
Enable, disable, and record removal each require literal `confirm: true` and the
host's `ExtensionControl` grant. Acquisition uses the separate
`ExtensionInstall` authority: a confirmed start returns a bounded job, status
reveals the resolved digest, manifest identity, and requested grants, and only a
second confirmed install or update echoes that observed revision and commits the
caller-selected grant; a changed candidate therefore makes stale consent fail. Cancellation
is also explicit and confirmed; no MCP call performs an unobservable blocking pull.
`husklet_extension_wait` follows the host's credit-controlled extension topics,
returning either the newest bounded inventory snapshot or acquisition job/revision
metadata. Acquisition notifications never carry manifest contents; clients fetch
status only after an invalidation, and coalescing is reported explicitly.

Volume and network inventory/inspection use the host's separate `VolumeRead`
and `NetworkRead` grants. Creation and attachment controls retain their
`VolumeWrite` or `NetworkWrite` grants. Volume/network removal and network
disconnect additionally require an explicit `confirm: true` MCP argument.

Image tools list and inspect local images under `ImageRead`, and pull under
`ImageWrite`. Removing an image or pruning unused images additionally requires
an explicit `confirm: true` MCP argument; the host still enforces `ImageWrite`.

`husklet_pane_read` is the single read path for agents that do not already know
what a pane holds. It inspects the split topology and returns one bounded XML
document: terminal panes include screen lines, focus, grid and tab metadata;
extension surfaces and the native `workspace` pane include their semantic tree.
It uses stable slots and semantic IDs, never screenshots, coordinates, or GTK
widget scraping. The older terminal-read and pane-snapshot tools remain for
consumers that need their specific typed result.

Workspace filesystem controls create one directory, rename without overwriting,
or remove one file or empty directory. All paths remain relative to declared
roots; removal requires `confirm: true`, and recursive deletion is unavailable.

Terminal layout tools use the same `beside`/`below` vocabulary as the host and
can focus, split, resize, rebalance, and close panes. Grid sizes and split ratios
are bounded; closing a pane requires an explicit `confirm: true` argument.

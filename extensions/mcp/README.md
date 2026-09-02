# @husklet/mcp

An MCP server for an LLM agent running as a Husklet extension. It connects only
to `HUSKLET_EXTENSION_SOCKET` and receives exactly the capabilities granted to
that extension.

```sh
npx @husklet/mcp
```

Tools use strict schemas and bounded, redacted results. Pane snapshots are
deterministic XML-like text carrying stable revisions, node IDs, roles, state,
and actions; other tools use JSON. Container exec and
terminal process spawning are deliberately absent: this package provides no
unrestricted shell shortcut. Pane semantic tools appear only when the installed
`@husklet/react` exposes the host-backed `terminal.semantics` and `terminal.act`
methods.

`husklet_pane_read` is the single read path for agents that do not already know
what a pane holds. It inspects the split topology and returns one bounded XML
document: terminal panes include screen lines, focus, grid and tab metadata;
extension surfaces and the native `workspace` pane include their semantic tree.
It uses stable slots and semantic IDs, never screenshots, coordinates, or GTK
widget scraping. The older terminal-read and pane-snapshot tools remain for
consumers that need their specific typed result.

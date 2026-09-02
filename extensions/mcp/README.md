# @husklet/mcp

An MCP server for an LLM agent running as a Husklet extension. It connects only
to `HUSKLET_EXTENSION_SOCKET` and receives exactly the capabilities granted to
that extension.

```sh
npx @husklet/mcp
```

Tools use strict schemas and bounded, redacted JSON results. Container exec and
terminal process spawning are deliberately absent: this package provides no
unrestricted shell shortcut. Pane semantic tools appear only when the installed
`@husklet/react` exposes the host-backed `terminal.semantics` and `terminal.act`
methods.

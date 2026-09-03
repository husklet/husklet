# @husklet/react

Write a workspace interface in React; the host renders it as native GTK widgets.
There is no DOM and no web view: components are the host's own component
library, and a React commit becomes one atomic frame of patches on a unix
socket.

## Using it

For local development, install the same ordinary React package an extension
image uses:

```sh
npm install @husklet/react react@18.3.1
```

The published base image is multi-architecture (`linux/amd64` and
`linux/arm64`) and already contains Node, React, the reconciler, and this SDK.
Start from the complete context shipped in this package:

```sh
cp -R node_modules/@husklet/react/examples/starter my-extension
cd my-extension
docker build -t my-extension .
```

[`examples/starter`](examples/starter) contains `main.js`, `extension.toml`,
and its Dockerfile. The manifest label names the file inside the image—the host
does not accept an inline placeholder—and `COPY --chown=node:node` preserves the
base image's non-root runtime. Pin `HUSKLET_REACT_IMAGE` to the release version
you tested before publishing an extension; `latest` is only the starter default.

```jsx
import React, { useState } from 'react';
import { connect, render, Column, Button, Text } from '@husklet/react';

function App() {
  const [count, setCount] = useState(0);
  return (
    <Column gap={2} pad={4}>
      <Text scale="title">Clicked {count} times</Text>
      <Button label="Go" tone="accent" onInvoke={() => setCount(count + 1)} />
    </Column>
  );
}

const session = await connect();          // reads HUSKLET_EXTENSION_SOCKET
render(<App />, session, { title: 'My Extension' });
```

## Pane providers

An extension can offer named views in the workspace pane chooser. Declare each
choice in the image manifest alongside the required `interface` capability:

```toml
capabilities = ["interface"]

[[pane_providers]]
id = "logs"
title = "Service logs"
icon = "text-x-generic-symbolic"
```

Choosing that entry sends a typed `PaneSelection` carrying both the provider ID
and the stable workspace slot that selected it. Use that event to select the
view rendered by the extension's existing root:

```jsx
import React from 'react';
import { connect, render, LogView, Text, usePaneSelection } from '@husklet/react';

const session = await connect();

function App() {
  const selection = usePaneSelection(session, 'logs');
  return selection
    ? <LogView value={`Logs selected in ${selection.slot}`} />
    : <Text value="Choose Service logs from a pane menu" />;
}

render(<App />, session, { title: 'My Extension' });
```

The host sends only providers declared by this extension. The `slot` lets state
and diagnostics remain pane-addressed; it is not a request to open an unrelated
tab or split. `usePaneSelection` removes its session observer on unmount and
when the session changes. `useHostEvents(session, listener)` provides the same
cleanup and fresh-callback behavior for other typed `HostEvent` handling.
Current interface events form a discriminated union on `interaction`: checking
for `"key"`, for example, makes `key`, `keycode`, `modifiers`, and `pressed`
required, while a pointer event exposes its finite phase vocabulary and nullable
coordinates. `LegacyInterfaceEvent` is kept separately for protocol-1 hosts
that used the older `event` envelope; new code should narrow `InterfaceEvent`.

## Workspace API

Host calls are promises with typed results and typed failures. Outstanding
calls are bounded; a missing ordered reply closes the session after the timeout
rather than risking correlation with the wrong caller.

```js
import { connect, workspace } from '@husklet/react';

const session = await connect({ timeout: 10_000, pendingLimit: 32 });
const host = workspace(session);
const configuration = await host.inspect('backend');
await host.stop('backend');
await host.update('backend', { ...configuration, memory_mb: 4096 });
await host.start('backend');
const containers = await host.containers.list();
await host.containers.stop(containers[0].id);
const pane = await host.containers.attachTerminal(containers[0].id, ['sh', '-i']);
const processes = await host.containers.processes(containers[0].id);
const output = await host.containers.logs(containers[0].id, { stderr: false });
const files = await host.files.list('project');
await host.files.mkdir('project/generated');
await host.files.write('project/generated/config.json', new TextEncoder().encode('{}'));
await host.files.rename('project/generated/config.json', 'project/generated/app.json');
```

Container reads include bounded logs, initial-process snapshots, and execution
inspection. Process PIDs are point-in-time display values and may be reused;
`stop`, `remove`, `kill`, and `exec` accept only complete immutable container IDs returned
by inventory or inspection; `signalExecution` likewise requires an immutable
execution ID; `removeExecution` requires that same complete immutable identity.
Names, prefixes, and snapshot PIDs remain useful only for bounded
lookup and display. The explicit control grant covers pause,
unpause, restart, kill, and detached `exec`. Image inspection and pulls may use
human tags, but removal requires the complete `sha256:` digest returned by
inventory so a moved tag cannot select a different image after confirmation.
Network inspection may use a canonical name, but remove, connect, and disconnect
require the complete 32-hex network ID from inventory. Attachment mutations also
require the complete immutable container ID.
`attachTerminal` requires the separate `container-attach` grant, preserves argv
boundaries, opens a non-restored GUI tab, and kills its interactive exec when that
pane disconnects.
Volume removal takes both its canonical name and the 32-hex `generation` returned
by inventory or inspection. The host compares that generation atomically, so a
removed and recreated same-name volume needs fresh consent.
The host currently publishes changed full snapshots for `containers`,
`images`, `volumes`, `networks`, and `terminal`. Start and stop those bounded, credit-controlled feeds
with `host.subscribe(topic)` and `host.unsubscribe(topic)`, and receive payloads
through `connect({ onEvent })` or `session.onEvent()`.

Terminal control is pane-addressed and promise-based as well. `terminal.read`
returns at most 2,000 lines with the cursor and grid dimensions from the same
authoritative screen snapshot; `terminal.writeInput` accepts at most 65,536 raw
bytes and appends nothing, and `terminal.resizeGrid` accepts dimensions from 1
through 1,000. `terminal.topology()` returns the current nested tab/split tree;
it is an observation call, not a claimed global change stream.

`protocolCoverage` is the machine-readable inventory of what this protocol
version really supports. Its image inventory includes the implemented bounded
list/inspect and pull calls plus digest-bound removal and confirmed prune
authority; callers do not need to infer those operations from TypeScript alone.
Workspace creation, configuration and lifecycle are
available under the explicit `workspace-control` grant. A running workspace
must be stopped before it is updated, and an extension cannot stop, restart or
delete the workspace hosting it. The `unavailable` section names remaining
areas such as drag/drop events;
those names deliberately are not callable methods. `Session.onEvent` is
low-level transport plumbing for events the host does send. Interface handlers
receive bounded key, focus, and pointer details, while the credit-controlled
`workspace-events` subscription carries workspace-level key, focus, and pointer
events. Pointer events identify the exact pane slot and occupant generation,
use pane-local coordinates, and distinguish motion, boundary, button, context,
and scroll phases with bounded modifiers and deltas. Neither is a promise that
every global workspace snapshot is published.

## Props

One component per tag — `<Card>`, `<Button>`, `<TableCell>`, 133 of them,
exported by name from the package root.

- **A property is its Rust name in camelCase.** `Label` is `label`, `RowSpan` is
  `rowSpan`. An unknown prop is an error, not a silent no-op.
- **The property decides the wire type, not the JavaScript value.** `gap={2}` is
  two spacing steps, `columns={2}` is two columns, `fraction={0.5}` is a number.
- **Closed vocabularies are written in kebab or camel case**: `tone="accent"`,
  `variant="outline"`, `scale="title"`, `align="center"`, `color="text-dim"`.
- **Lengths**: a number is steps on the 4px scale; `"fill"` and `"content"` are
  the named sizes; `{chars: 12}` is a text-relative width. `pad` also takes
  `{top, end, bottom, start}`, and `width`/`height` take `{minimum, maximum}`.
- **`null` or `undefined` means the host should forget the property** — it emits
  `ClearProp`.
- **Text children are the label.** `<Text>hello</Text>` and
  `<Text label="hello" />` are the same thing; bare text has no widget.
- **A handler is `on` plus the trigger**: `onInvoke`, `onChange`, `onSubmit`,
  `onSelect`, `onActivate`, `onToggle`, `onExpand`, `onScroll`, `onClose`,
  `onContext`. The event identity is derived from the node and the trigger, so
  re-rendering with a fresh closure rebinds locally and sends no patch. The
  callback receives `{trigger, node, id, value}`.

`vocabulary` exports both lists, and `tags` exports every component name.

### Terminal transcript

`TerminalTranscript` is a native, selectable text projection for terminal
inspection. It accepts string lines or `{id, number, text, timestamp, stream,
tone}` records, an optional `{line, column}` cursor, line-number/timestamp
toggles, and bounded explicit actions. The component keeps the newest 256
lines, no more than 2,048 UTF-8 bytes per line or 65,536 bytes overall, and
shows when content was truncated. `onSelect` receives the retained line rather
than asking consumers to recover identity from rendered text.

### Command palette

`CommandPaletteView` composes the native command input, grouped result list,
empty state, and semantic buttons into a keyboard-first picker. It fuzzy
matches titles, groups, and keywords; preserves stable command IDs; skips
disabled commands during keyboard traversal; and forwards destructive metadata
to automation. It accepts at most 256 commands, 128 UTF-8 query bytes, and 256
UTF-8 bytes for displayed command fields. The lower-level `CommandPalette`
input remains exported for custom compositions.

## Tests

`npm test` — plain `node --test`, no framework.

`npm run pack:check` — checks the exact npm tarball allowlist, installs it into
a temporary consumer, imports its runtime entry, type-checks a consumer, and
statically verifies the multi-architecture/non-root base-image contract. A real
container build still requires Docker or another OCI builder.

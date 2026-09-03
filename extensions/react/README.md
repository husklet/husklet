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
npm install
npm test
docker build -t my-extension .
```

[`examples/starter`](examples/starter) is a standalone Node project containing
`package.json`, `main.js`, `extension.toml`, and its Dockerfile. Its SDK dependency
is bound to the same version as the package that supplied the starter. The manifest
label names the file inside the image—the host
does not accept an inline placeholder—and `COPY --chown=node:node` preserves the
base image's non-root runtime. The starter defaults to the version-matched
base-image tag released with this SDK; override `HUSKLET_REACT_IMAGE`
deliberately when testing against another runtime, or pin that argument to a
registry digest when the extension build itself must be immutable.

This is the one published Husklet SDK base image. It contains both
`@husklet/client` and `@husklet/react`; the framework-neutral client starter
instead builds directly from pinned Node and copies its locally installed
client package. Neither Dockerfile resolves npm during its final image build.
An offline OCI build still requires the selected Node or React base image to
already exist in the builder's cache.

The repository's `extensions/react/Dockerfile` is release infrastructure, not
a standalone file in the npm tarball: its build context deliberately requires
the sibling client and React source trees. The npm package instead ships the
complete `examples/starter` Docker context, which consumes the published base
image without depending on monorepo-private files.

`npm start` reports missing sockets, handshake failures, and unexpected host EOF
on stderr and exits nonzero. `SIGINT` and `SIGTERM` close an established session
without reporting a false failure. It never reconnects automatically: the socket
is an authority credential, so a replacement must be launched and granted by the
host. The host validates `extension.toml` before starting the image; `npm start`
alone exercises the entrypoint and cannot validate image manifest registration.

```js
import React, { useState } from 'react';
import { Button, Column, Text, connect, render } from '@husklet/react';

function App() {
  const [count, setCount] = useState(0);
  return React.createElement(Column, { gap: 2, pad: 4 },
    React.createElement(Text, { label: `Clicked ${count} times`, scale: 'title' }),
    React.createElement(Button, { label: 'Go', tone: 'accent', onInvoke: () => setCount(count + 1) }));
}

const session = await connect();          // reads HUSKLET_EXTENSION_SOCKET
render(React.createElement(App), session, { title: 'My Extension' });
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

```js
import React from 'react';
import { connect, render, LogView, Text, usePaneSelection } from '@husklet/react';

const session = await connect();

function App() {
  const selection = usePaneSelection(session, 'logs');
  return selection
    ? React.createElement(LogView, { value: `Logs selected in ${selection.slot}` })
    : React.createElement(Text, { label: 'Choose Service logs from a pane menu' });
}

render(React.createElement(App), session, { title: 'My Extension' });
```

The host sends only providers declared by this extension. The `slot` lets state
and diagnostics remain pane-addressed; it is not a request to open an unrelated
tab or split. `usePaneSelection` removes its session observer on unmount and
when the session changes. `useHostEvents(session, listener)` provides the same
cleanup and fresh-callback behavior for other typed `HostEvent` handling.
Current interface events form a discriminated union on `interaction`: checking
for `"key"`, for example, makes `key`, `keycode`, `modifiers`, and `pressed`
required, while a pointer event exposes its finite phase vocabulary and nullable
coordinates. A `Container` can bind `onDrag` and `onDrop` for in-process
reordering: drop reports only the bounded source node identity and local `x`/`y`
coordinates. Arbitrary files, MIME data, clipboard contents, and cross-process
payloads are deliberately not exposed. `LegacyInterfaceEvent` is kept separately for protocol-1 hosts
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
if (!configuration.generation) throw new Error('inspection omitted workspace generation');
await host.stop('backend');
await host.update('backend', configuration.generation, { ...configuration, memory_mb: 4096 });
await host.start('backend');
const containers = await host.containers.list();
const created = await host.containers.create({
  image: 'alpine:3.20',
  name: 'worker',
  command: ['sleep', '300'],
  environment: [['MODE', 'development']],
  mounts: [{ volume: 'build-cache', target: '/cache', read_only: false }],
  ports: [{ container: 8080, host: null, protocol: 'tcp' }],
  memory_mb: 512,
  cpus: 2,
  pids_limit: 128,
});
await host.containers.start(created);
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
inspection. A process snapshot names the complete immutable container ID actually
sampled; its PIDs remain point-in-time display values and may be reused;
`start`, `stop`, `pause`, `unpause`, `restart`, `remove`, `kill`, and `exec`
accept only complete immutable container IDs returned by inventory or inspection.
Execution inspection, output replay, waiting, signaling, and removal all require
the complete immutable execution ID returned by the bounded execution catalogue.
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
The host publishes bounded change feeds for container and execution inventories,
image pulls, panes, extensions and their acquisitions, workspace lifecycle and
input events, plus the legacy image, volume, network, and terminal snapshots. Start and stop those credit-controlled feeds
with `host.subscribe(topic)` and `host.unsubscribe(topic)`, and receive payloads
through `connect({ onEvent })` or `session.onEvent()`. An acknowledged final
unsubscribe retires the host channel and discards any coalesced snapshot, so
later subscriptions start with fresh credit and state.

Terminal control is pane-addressed and promise-based as well. `terminal.read`
returns at most 2,000 lines with the cursor and grid dimensions from the same
authoritative screen snapshot; `terminal.splitObserved(slot, generation, revision, division)`
splits only that exact snapshot (the legacy `split` remains for compatibility);
`terminal.spawnObserved(slot, generation, revision, argv)` similarly prevents a
stale slot from running argv in a replacement terminal (legacy `spawn` remains);
`terminal.ratioObserved` binds layout resizing to the same cursor while legacy
`ratio` remains available;
`terminal.resizeGridObserved` binds PTY resizing to the same cursor while legacy
`resizeGrid` remains available;
`terminal.writeInput` accepts at most 65,536 raw
bytes and appends nothing, and `terminal.resizeGrid` accepts dimensions from 1
through 1,000. `terminal.closeObserved(slot, generation, revision)` closes only
the exact pane snapshot returned by inventory/read; legacy `close(slot)` remains
for compatibility. `terminal.topology()` returns the current nested tab/split tree;
it is an observation call, not a claimed global change stream.
Use `terminal.switchOccupantObserved(slot, generation, revision, target)` for an
observe-then-switch workflow; the generation-only method remains for compatibility.

`protocolCoverage` is the machine-readable inventory of what this protocol
version really supports. Its image inventory includes the implemented bounded
list/inspect and pull calls plus digest-bound removal and confirmed prune
authority; callers do not need to infer those operations from TypeScript alone.
Workspace creation, configuration and lifecycle are
available under the explicit `workspace-control` grant. A running workspace
must be stopped before it is updated, and an extension cannot stop, restart or
delete the workspace hosting it. Names under `unavailable` deliberately are not
callable methods. `Session.onEvent` is
low-level transport plumbing for events the host does send. Interface handlers
receive bounded key, focus, and pointer details, while the credit-controlled
`workspace-events` subscription carries workspace-level key, focus, and pointer
activity. Its `dropped` count includes queue pressure, motion coalescing, and
native callback contention, so consumers can detect every observation gap.
Pointer events identify the exact pane slot and occupant generation,
use pane-local coordinates, and distinguish motion, boundary, button, context,
and scroll phases with bounded modifiers and deltas. Key and window-focus
events also carry the focused terminal slot/generation when a terminal owns
focus, or explicit null identity while window chrome owns it. Neither is a promise that
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
  `onSelect`, `onEdit`, `onSort`, `onActivate`, `onToggle`, `onExpand`, `onScroll`, `onClose`,
  `onContext`. The event identity is derived from the node and the trigger, so
  re-rendering with a fresh closure rebinds locally and sends no patch. The
  callback receives `{trigger, node, id, value}`.

Editable virtual tables remain producer-controlled. Mark only intended columns
with `{editable: true}` and handle `onEdit` as a proposal carrying
`{source, version, row: {index, id}, column, value}`. Compare `source` and
`version` with the current collection and use the immutable row `id`; reject a
stale proposal without mutating data. The native cell restores its authoritative
bound value immediately. After validation and persistence succeed, advance the
source version and publish a new bounded row window to display the accepted
value. Show rejection feedback separately rather than leaving an uncommitted
draft in the table.

Sortable virtual tables use the same authority rule. Mark only intended columns
with `{sortable: true}` and handle `onSort` as a proposal carrying
`{source, version, column, descending}`. Refuse a stale version or an undeclared
column, then publish a newer source version after accepting the order. The native
header shows direction but never locally reorders partial virtual windows.

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

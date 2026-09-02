# @husklet/react

Write a workspace interface in React; the host renders it as native GTK widgets.
There is no DOM and no web view: components are the host's own component
library, and a React commit becomes one atomic frame of patches on a unix
socket.

## Using it

```dockerfile
FROM husklet/react:latest
COPY . /app
CMD ["node", "/app/main.js"]
LABEL husklet.extension.protocol="1"
LABEL husklet.extension.manifest="{...}"
```

```jsx
import { connect, render, Column, Button, Text } from '@husklet/react';

function App() {
  const [count, setCount] = React.useState(0);
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
const processes = await host.containers.processes(containers[0].id);
const output = await host.containers.logs(containers[0].id, { stderr: false });
```

Container reads include bounded logs, process tables, and execution inspection;
the explicit control grant covers pause, unpause, restart, kill, and detached
`exec`. The host currently publishes changed full snapshots for `containers`,
`images`, `volumes`, `networks`, and `terminal`. Start and stop those bounded, credit-controlled feeds
with `host.subscribe(topic)` and `host.unsubscribe(topic)`, and receive payloads
through `connect({ onEvent })` or `session.onEvent()`.

Terminal control is pane-addressed and promise-based as well. `terminal.read`
returns at most 2,000 lines, `terminal.writeInput` accepts at most 65,536 raw
bytes and appends nothing, and `terminal.resizeGrid` accepts dimensions from 1
through 1,000. `terminal.topology()` returns the current nested tab/split tree;
it is an observation call, not a claimed global change stream.

`protocolCoverage` is the machine-readable inventory of what this protocol
version really supports. Workspace creation, configuration and lifecycle are
available under the explicit `workspace-control` grant. A running workspace
must be stopped before it is updated, and an extension cannot stop, restart or
delete the workspace hosting it. The `unavailable` section names remaining
areas such as extension snapshots and keyboard events;
those names deliberately are not callable methods. `Session.onEvent` is
low-level transport plumbing for events the host does send, not a promise that
global workspace snapshots are published.

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

## Tests

`npm test` — plain `node --test`, no framework.

`npm run pack:check` — checks the exact npm tarball allowlist, installs it into
a temporary consumer, imports its runtime entry, type-checks a consumer, and
statically verifies the multi-architecture/non-root base-image contract. A real
container build still requires Docker or another OCI builder.

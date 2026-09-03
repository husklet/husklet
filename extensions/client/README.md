# @husklet/client

Framework-neutral JavaScript and TypeScript access to the capability-scoped Husklet extension host.

```js
import { connect, workspace } from '@husklet/client';
const session = await connect();
console.log(await workspace(session).info());
```

The packaged `examples/agent-control.mjs` is an executable end-to-end consumer:
it discovers an exact terminal and UI pane, reads terminal text and semantic XML,
writes exact byte values, and invokes an advertised revision-bound UI action.
It accepts one JSON argument containing `path`, `terminalSlot`, `uiSlot`, `node`,
and an `input` byte array; it uses no renderer or private transport imports.

`examples/agent-container-control.mjs` is the corresponding bounded container
workflow. It inspects the workspace and an immutable container ID, starts only
when initially stopped, executes exact argv, waits and reads bounded output,
removes the completed execution record, and restores the container's initial
stopped state. A failed wait/log operation remains recoverable through
`ExecutionOperationError.executionId`; the example does not auto-remove it.

`examples/agent-workspace-control.mjs` proves administrative create/start/stop/
delete for a unique workspace name. It acknowledges lifecycle observation before
each mutation, requires strictly increasing matching revisions, and deletes with
the immutable generation returned by creation. It does not operate on the
extension's hosting workspace.

Every discovered pane has one framework-neutral text projection. Terminal panes
return their interpreted visible screen and cursor snapshot; native and extension
UI panes return bounded semantic XML:

```js
const host = workspace(session);
const pane = (await host.terminal.panes()).panes[0];
const readable = await host.terminal.toText(pane.slot, { lines: 200 });
console.log(readable.kind, readable.text);
const next = await host.terminal.waitForText(pane.slot, readable.snapshot, { timeoutMs: 10_000 });
if (next.changed) console.log(next.readable.text);

// Arm observation before a revision-bound UI action so its update cannot be missed.
const acted = await host.terminal.actAndWait(pane.slot, {
  generation: readable.snapshot.generation,
  revision: readable.snapshot.revision,
  node: 7,
  action: 'invoke',
});
if (acted.changed) console.log(acted.readable.text);
```

When starting from a node ID rather than an already-inspected action cursor,
validate the node against the live bounded tree before invoking it:

```js
const result = await host.terminal.inspectAndAct(pane.slot, { node: 7, action: 'invoke' });
console.log(result.before.text);
if (result.changed) console.log(result.after.text);
```

This refuses missing, truncated-away, disabled, or non-advertised actions before
authority. The action is bound to the generation and revision read after the
subscription was armed.

Extension acquisition also has a non-polling cursor. Read status once, then arm
the exact job before waiting for its next coalesced revision:

```js
const current = await host.extensions.acquisition(job);
const next = await host.extensions.waitForAcquisition(job, current.revision);
if (next.changed) console.log(next.status.state, next.status.progress);
```

Enable an installed digest and observe its durable roster state without racing
the inventory subscription:

```js
const enabled = await host.extensions.enableAndWait(extension.name, extension.image_digest);
if (enabled.changed) console.log(enabled.extension.status);

const disabled = await host.extensions.disableAndWait(extension.name, extension.image_digest);

// Retry a faulted installation. Inventory is armed before authority is invoked.
const retried = await host.extensions.retryAndWait(extension.name, extension.image_digest);
if (disabled.changed) console.log(disabled.extension.status); // durable standby; provider withdrawal is observed separately

const removed = await host.extensions.removeAndWait(extension.name, extension.image_digest);

// Consent is bound to this ready revision. Inventory is armed before commit.
const installed = await host.extensions.installAndWait(status.job, status.revision, status.candidate.requested);

// Arm observation before starting; an unchanged initial snapshot cannot settle this.
const running = await host.containers.startAndWait(containerId);
const exited = await host.containers.stopAndWait(containerId);
const removed = await host.containers.removeAndWait(containerId); // absence requires complete inventory
const restarted = await host.containers.restartAndWait(container.id, container.generation);
if (removed.changed) console.log(removed.replacement); // null, or a newly installed digest under the same name
```

Execute without a shell string, wait for completion, and fetch selected bounded
output streams in one ordered composition:

```js
const { execution, output } = await host.containers.execAndWait(container.id, {
  command: ['printf', 'ok'], timeoutMs: 10_000, stdout: true, stderr: false,
});
```

All options are validated before creation. A wait or log failure throws
`ExecutionOperationError` with the retained `executionId` and failing `phase`;
the client never removes that execution automatically.

Signals can be coupled to an exact observed execution transition. The default
waits for exit; use `state: 'changed'` when a non-terminating signal has a
catalogue-visible transition:

```js
const execution = await host.containers.execution(executionId);
const stopped = await host.containers.signalExecutionAndWait(
  execution.id, 'TERM', execution, { state: 'exited', timeoutMs: 10_000 },
);
```

Pane occupant changes can likewise be armed and verified without racing a raw
subscription against the mutation:

```js
const switched = await host.terminal.switchOccupantAndWait(
  pane.slot, pane.generation, pane.revision,
  { kind: 'surface', extension: 'workspace-manager', provider: 'main' },
);
if (switched.changed) console.log(switched.pane.provider);
```

Layout creation has the same arm-before-authority form. The source cursor comes
from pane inventory, and the successful result is the newly created pane:

```js
const split = await host.terminal.splitAndWait(
  pane.slot, pane.generation, pane.revision, 'beside', { timeoutMs: 10_000 },
);
if (split.changed) console.log(split.pane.slot);
```

Destructive close uses complete pane inventory as its absence authority. A
truncated inventory never settles the operation, and replacement of the slot is
reported instead of being mistaken for successful absence:

```js
const closed = await host.terminal.closeAndWait(
  pane.slot, pane.generation, pane.revision, { timeoutMs: 10_000 },
);
if (!closed.changed) console.log('close was accepted but absence was not observed');
```

Retitling can likewise verify the exact requested title rather than treating the
mutation acknowledgement as proof that an observer has caught up:

```js
const retitled = await host.terminal.retitleAndWait(
  pane.slot, pane.generation, pane.revision, 'Build logs', { timeoutMs: 10_000 },
);
if (retitled.changed) console.log(retitled.pane.title);
```

Focus also has an observable form that verifies the same pane generation is
reported focused at a newer revision:

```js
const focused = await host.terminal.focusAndWait(
  pane.slot, pane.generation, pane.revision, { timeoutMs: 10_000 },
);
if (focused.changed) console.log(focused.pane.focused); // true
```

Terminal input can be coupled to a screen transition without claiming that the
bytes produce any particular text. The helper subscribes and verifies the
supplied screen cursor before writing, then returns the later bounded screen:

```js
const written = await host.terminal.writeAndWait(
  pane.slot, screen.generation, screen.revision, new Uint8Array([0x03]),
  { lines: 200, timeoutMs: 10_000 },
);
if (written.changed) console.log(written.after.lines.join('\n'));
```

Spawning exact argv has the same observable form. Success means the bounded
terminal projection advanced; it does not claim that a silent command emitted
particular text or reached application-specific readiness:

```js
const spawned = await host.terminal.spawnAndWait(
  pane.slot, screen.generation, screen.revision, ['htop'],
  { lines: 200, timeoutMs: 10_000 },
);
if (spawned.changed) console.log(spawned.after.lines.join('\n'));
```

Opening the extension session's tab can be observed without matching its mutable
title. The host-returned tab identity is verified against bounded pane inventory:

```js
const opened = await host.terminal.openTabAndWait('Agent tools', { timeoutMs: 10_000 });
if (opened.changed) console.log(opened.tab, opened.pane.slot);
```

For a framework-neutral extension, copy the complete starter from the installed
package. It contains no React dependency or monorepo-relative import:

```sh
cp -R node_modules/@husklet/client/examples/starter my-extension
cd my-extension
npm install
npm test
npm start
```

`npm start` reads `HUSKLET_EXTENSION_SOCKET`, reports startup or unexpected host
closure on stderr, and closes cleanly on `SIGINT` or `SIGTERM`. `npm install` is
also the image-preparation step: the Dockerfile copies that exact installed,
dependency-free client instead of resolving npm again during the build. It uses
a digest-pinned Node image and runs the extension as the non-root `node` user.
The image label points at the included manifest; the host validates that
manifest when installing the image.

There is no separate published client-only Husklet base image. The published
`extension-react-base` contains both SDK packages; this framework-neutral
starter stays smaller by using pinned Node and copying the exact client from
`npm install`. Its image build performs no npm registry resolution, but a fully
offline OCI build still requires the pinned Node base to be present in the
builder's cache.

The client normally allows 30 seconds for the host's opening handshake. Set
`HUSKLET_EXTENSION_CONNECT_TIMEOUT_MS` to a positive millisecond value when a
development or test environment needs a shorter, explicit startup deadline.

The transport is a persistent, full-duplex, length-prefixed Unix stream. Calls are correlated while bounded host events can arrive independently with explicit subscription credit. This is WebSocket-like interaction, but it is **not a WebSocket**. The handshake returns workspace identity and negotiated capability grants. All methods remain constrained to that authority.

After `connect()` resolves, `session.grantedCapabilities` is an immutable array
of the exact Rust capability wire names. Calls and subscriptions lacking one of
those negotiated grants reject locally without writing a request frame. The host
still checks every request authoritatively, including changes in authority after
the handshake.

`@husklet/react` consumes and re-exports this client for compatibility.

Low-level calls accept an optional `AbortSignal` as the third argument:

```js
await session.call('workspace_info', undefined, { signal });
```

The complete typed facade can be bound without changing individual method
signatures. This applies to nested container, terminal, filesystem, and other
methods as well:

```js
const cancellable = workspace(session).withSignal(signal);
await cancellable.containers.inspect(containerId);
```

When a bound watcher is already active, abort removes its listener and releases
its reference-counted host subscription. Aborting while its subscribe call is
still pending follows the ordered-call fail-closed rule below.

An already-aborted signal rejects without writing. Because protocol v1 orders
replies on one channel and has no request identifiers, aborting after a request
was written closes the session and rejects every pending call; this prevents a
late reply from being delivered to the wrong caller. Create a new session after
such a cancellation.

`protocolSurface` is the machine-readable public-surface inventory derived from
the authoritative Rust schema. `protocolSurface.requests` names the typed
`workspace(session)` method for every ordinary host request; renderer-owned
framing calls carry an explicit internal rationale. `protocolSurface.topics`
maps every host topic to the typed `subscribe(topic)` / `unsubscribe(topic)`
API, so extensions do not need to discover normal operations through
`Session.call()`.

Use the typed inventory watchers when consuming host snapshots: `watchImages`,
`watchVolumes`, `watchNetworks`, and `watchTerminal` deliver the bounded types
from the Rust protocol schema and return an async disposer. The disposer sends
the matching unsubscribe when the final local listener stops; event credit is
returned only after the validated snapshot has been delivered.

See [API.md](API.md) for the complete schema-checked method and topic reference,
including capability requirements, observation identities, bounds, errors, and
short client-only examples.

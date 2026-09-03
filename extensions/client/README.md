# @husklet/client

Framework-neutral JavaScript and TypeScript access to the capability-scoped Husklet extension host.

```js
import { connect, workspace } from '@husklet/client';
const session = await connect();
console.log(await workspace(session).info());
```

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

Extension acquisition also has a non-polling cursor. Read status once, then arm
the exact job before waiting for its next coalesced revision:

```js
const current = await host.extensions.acquisition(job);
const next = await host.extensions.waitForAcquisition(job, current.revision);
if (next.changed) console.log(next.status.state, next.status.progress);
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

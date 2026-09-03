# @husklet/client

Framework-neutral JavaScript and TypeScript access to the capability-scoped Husklet extension host.

```js
import { connect, workspace } from '@husklet/client';
const session = await connect();
console.log(await workspace(session).info());
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
closure on stderr, and closes cleanly on `SIGINT` or `SIGTERM`. Its Dockerfile
uses a digest-pinned Node image, installs the committed lock as root, then runs
the extension as the non-root `node` user. The image label points at the included
manifest; the host validates that manifest when installing the image.

The transport is a persistent, full-duplex, length-prefixed Unix stream. Calls are correlated while bounded host events can arrive independently with explicit subscription credit. This is WebSocket-like interaction, but it is **not a WebSocket**. The handshake returns workspace identity and negotiated capability grants. All methods remain constrained to that authority.

After `connect()` resolves, `session.grantedCapabilities` is an immutable array
of the exact Rust capability wire names. Calls and subscriptions lacking one of
those negotiated grants reject locally without writing a request frame. The host
still checks every request authoritatively, including changes in authority after
the handshake.

`@husklet/react` consumes and re-exports this client for compatibility.

`protocolSurface` is the machine-readable public-surface inventory derived from
the authoritative Rust schema. `protocolSurface.requests` names the typed
`workspace(session)` method for every ordinary host request; renderer-owned
framing calls carry an explicit internal rationale. `protocolSurface.topics`
maps every host topic to the typed `subscribe(topic)` / `unsubscribe(topic)`
API, so extensions do not need to discover normal operations through
`Session.call()`.

See [API.md](API.md) for the complete schema-checked method and topic reference,
including capability requirements, observation identities, bounds, errors, and
short client-only examples.

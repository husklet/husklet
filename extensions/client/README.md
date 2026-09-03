# @husklet/client

Framework-neutral JavaScript and TypeScript access to the capability-scoped Husklet extension host.

```js
import { connect, workspace } from '@husklet/client';
const session = await connect();
console.log(await workspace(session).info());
```

The transport is a persistent, full-duplex, length-prefixed Unix stream. Calls are correlated while bounded host events can arrive independently with explicit subscription credit. This is WebSocket-like interaction, but it is **not a WebSocket**. The handshake returns workspace identity and negotiated capability grants. All methods remain constrained to that authority.

`@husklet/react` consumes and re-exports this client for compatibility.

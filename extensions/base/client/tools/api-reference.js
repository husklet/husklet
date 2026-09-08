import assert from 'node:assert/strict';
import fs from 'node:fs';
import { fileURLToPath } from 'node:url';

import {
  PROTOCOL_BOUNDS,
  PROTOCOL_TOPICS,
  protocolSurface,
  requestCapability,
} from '../dist/index.js';

const output = fileURLToPath(new URL('../API.md', import.meta.url));
const execution = new Set([
  'processes',
  'execution',
  'executions',
  'executionLogs',
  'executionOutput',
  'waitExecution',
  'signalExecution',
  'cancelExecution',
  'removeExecution',
]);
const semantic = new Set(['semantics', 'act']);
const groups = new Map([
  ['Workspace', []],
  ['Containers', []],
  ['Processes and executions', []],
  ['Terminal and panes', []],
  ['Files', []],
  ['Private extension state', []],
  ['Images', []],
  ['Networks', []],
  ['Volumes', []],
  ['Extensions', []],
  ['Notifications', []],
  ['Semantics', []],
]);

for (const [wire, route] of Object.entries(protocolSurface.requests)) {
  if (route.kind !== 'facade') continue;
  const [namespace, method] = route.api.includes('.')
    ? route.api.split('.')
    : ['workspace', route.api];
  const group =
    namespace === 'containers' && execution.has(method)
      ? 'Processes and executions'
      : namespace === 'terminal' && semantic.has(method)
        ? 'Semantics'
        : {
            workspace: 'Workspace',
            containers: 'Containers',
            terminal: 'Terminal and panes',
            files: 'Files',
            state: 'Private extension state',
            images: 'Images',
            networks: 'Networks',
            volumes: 'Volumes',
            extensions: 'Extensions',
            notifications: 'Notifications',
          }[namespace];
  assert(group, `no documentation group for ${wire}`);
  groups
    .get(group)
    .push(wire === 'notification_publish'
      ? '- `host.notifications.publish(...)` — queues a bounded, extension-attributed OS notification; the reply acknowledges host acceptance, not platform delivery; requires `notifications:publish`.'
      : `- \`host.${route.api}(...)\` — \`${wire}\`, requires \`${requestCapability(wire)}\`.`);
}
groups
  .get('Terminal and panes')
  .push(
    '- `host.terminal.toText(...)` — discovers a pane and returns visible terminal screen text or bounded semantic XML; requires `panes:observe` and the corresponding `terminals:output` or `panes:semantic-read` grant.',
    '- `host.terminal.readAll(...)` — discovers panes once and converts each to terminal transcript or bounded semantic XML, reports incomplete discovery, and refuses cursor races; requires `panes:observe`, `terminals:output`, and `panes:semantic-read` for mixed workspaces.',
    '- `host.terminal.waitForText(...)` — arms pane-change observation, ignores the unchanged cursor, then returns a fresh bounded text projection; requires `panes:observe` and the corresponding read grant.',
    '- `host.terminal.actAndWait(...)` — arms pane observation before a revision-bound semantic action, then returns its changed bounded projection; requires `panes:observe`, `panes:semantic-control`, and the corresponding read grant.',
    '- `host.terminal.switchOccupantAndWait(...)` — arms observation before an observed occupant switch and verifies the exact terminal or extension/provider identity; requires `panes:observe` and `terminals:control`.',
    '- `host.terminal.splitAndWait(...)` — arms pane changes before a generation/revision-bound split and verifies the returned child slot from bounded inventory; requires `panes:observe` and `terminals:control`.',
    '- `host.terminal.closeAndWait(...)` — arms pane changes before a generation/revision-bound close and proves absence only from a complete pane inventory; requires `panes:observe` and `terminals:control`.',
    '- `host.terminal.retitleAndWait(...)` — arms pane changes before a generation/revision-bound retitle and verifies the exact title at an advanced revision; requires `panes:observe` and `terminals:control`.',
    '- `host.terminal.focusAndWait(...)` — arms pane changes before generation/revision-bound focus and verifies the same pane is focused at an advanced revision; requires `panes:observe` and `terminals:control`.',
    '- `host.terminal.writeAndWait(...)` — arms and reads the exact terminal screen cursor before writing bounded bytes, then returns a later bounded screen revision; requires `panes:observe`, `terminals:output`, and `terminals:control`.',
    '- `host.terminal.spawnAndWait(...)` — arms and reads the exact terminal screen cursor before a generation/revision-bound argv spawn, then returns a later bounded screen revision; requires `panes:observe`, `terminals:output`, and `terminals:control`.',
    '- `host.terminal.resizeGridAndWait(...)` — arms and reads the exact terminal screen cursor before a generation/revision-bound resize, then verifies the requested columns and rows on a later screen revision; requires `panes:observe`, `terminals:output`, and `terminals:control`.',
    '- `host.terminal.ratioAndWait(...)` — arms pane observation before a generation/revision-bound ratio change, then verifies the advanced pane and resulting topology (allowing host pixel quantization); requires `panes:observe`, `terminals:read`, and `terminals:control`.',
    '- `host.terminal.openTabAndWait(...)` — arms pane observation before opening the session-owned tab and verifies a pane under the exact returned tab identity; post-creation observation failures retain `{ tab, title }` in `TerminalOperationError`; requires `panes:observe` and `terminals:control`.',
  );
groups
  .get('Private extension state')
  .push(
    '- `host.state.readJson(codec)` / `writeJson(observed, value, codec)` — decode and encode the bounded blob through an extension-owned runtime validator/migrator.',
    '- `host.state.updateJson(codec, update, { attempts })` — retries only CAS conflicts (up to 16 attempts); `update` may run more than once and must be safe to repeat.',
  );
groups
  .get('Semantics')
  .push(
    '- `host.terminal.inspectAndAct(slot, proposal, options)` — arms observation, reads the bounded semantic tree, verifies an enabled advertised node action, invokes it at that exact revision, and returns bounded XML before/after; requires `panes:observe`, `panes:semantic-read`, and `panes:semantic-control`.',
  );
groups
  .get('Extensions')
  .push(
    '- `host.extensions.waitForAcquisition(...)` — waits for an exact acquisition job revision to advance, then reads its authoritative full status; requires `extensions:install`.',
    '- `host.extensions.enableAndWait(...)` — arms inventory before enabling an exact installed digest, then verifies its durable enabled state; requires `extensions:read` and `extensions:control`.',
    '- `host.extensions.disableAndWait(...)` — arms inventory before disabling an exact installed digest, then verifies durable standby; provider withdrawal remains separately observable; requires `extensions:read` and `extensions:control`.',
    '- `host.extensions.retryAndWait(...)` — arms inventory before retrying an exact faulted digest, rejects replacement/disappearance, then verifies durable duty; requires `extensions:read` and `extensions:control`.',
    '- `host.extensions.removeAndWait(...)` — arms inventory before removing an exact installed digest, then proves that digest is absent and reports any same-name replacement; requires `extensions:read` and `extensions:control`.',
    '- `host.extensions.installAndWait(...)` / `updateAndWait(...)` — inspect the exact ready acquisition revision, send its reviewed immutable digest as commit CAS authority, arm inventory before commit, and verify the returned and published name/digest; requires `extensions:install` and `extensions:read`.',
    '- `host.containers.startAndWait(...)` — acknowledges bounded inventory before starting an immutable ID, ignores the unchanged initial snapshot, and returns only on a later running state; requires `containers:read` and `containers:control`.',
    '- `host.containers.stopAndWait(...)` — acknowledges bounded inventory before stopping an immutable ID, ignores unchanged/running snapshots, and returns only on a later exited state; requires `containers:read` and `containers:control`.',
    '- `host.containers.removeAndWait(...)` — arms an explicit completeness-bearing inventory before removal and accepts absence only from a later `complete: true` snapshot; requires `containers:read` and `containers:control`.',
    '- `host.containers.restartAndWait(...)` — arms inventory before restarting an immutable ID and accepts only `running` at a generation newer than the caller observed; requires `containers:read` and `containers:control`.',
  );
groups
  .get('Processes and executions')
  .push(
    '- `host.containers.execAndWait(id, options)` — prevalidates bounded execution/output options, executes by immutable container ID, waits, then fetches bounded logs; failures retain the execution ID, and log-phase failures retain the authoritative completed summary, in `ExecutionOperationError`; records are never auto-removed.',
    '- `host.containers.signalExecutionAndWait(id, signal, after, options)` — arms execution observation, verifies the immutable execution cursor, signals, then awaits an explicit changed or exited state; requires `containers:read` and `containers:control`.',
  );

const topicCapability = Object.fromEntries(
  PROTOCOL_TOPICS.map(({ wire, capability }) => [wire, capability]),
);
const sections = [...groups]
  .map(([heading, operations]) => `## ${heading}\n\n${operations.join('\n')}`)
  .join('\n\n');
const events = Object.keys(protocolSurface.topics)
  .map(
    (topic) =>
      `- \`host.subscribe('${topic}')\` / \`host.unsubscribe('${topic}')\` — requires \`${topicCapability[topic]}\`.`,
  )
  .join('\n');
const internal = Object.entries(protocolSurface.requests)
  .filter(([, route]) => route.kind === 'internal')
  .map(([wire, route]) => `- \`${wire}\` — ${route.rationale}.`)
  .join('\n');
const bounds = Object.entries(PROTOCOL_BOUNDS)
  .map(([name, value]) => `- \`${name}\`: ${value}`)
  .join('\n');

const reference = `# @husklet/client API reference

This reference is generated from the public \`protocolSurface\`, which is itself
closed over the authoritative Rust protocol schema. A stale operation, topic, or
capability makes \`npm test\` fail; regenerate intentionally with \`npm run api:generate\`.

Create one typed facade and reuse it:

\`\`\`js
import { connect, workspace } from '@husklet/client';
const session = await connect({ timeout: 10_000, pendingLimit: 32 });
const host = workspace(session);
const panes = await host.terminal.panes();
const readable = await host.terminal.toText(panes.panes[0].slot, { lines: 200 });
console.log(readable.text);
const next = await host.terminal.waitForText(panes.panes[0].slot, readable.snapshot);
if (next.changed) console.log(next.readable.text);
await session.close();
\`\`\`

Every call is checked against the capabilities granted by the handshake. A denied,
absent, conflicting, failed, or unsupported host reply rejects with \`ExtensionError\`;
branch on \`error.kind\`, not message text. Pending calls are bounded and time out by
closing the ordered session, because continuing could attach a later reply to the
wrong caller.

## Extension feasibility

| Extension shape | Current fit | Relevant API and remaining constraint |
| --- | --- | --- |
| Code/embedding index | Strong | A bounded, completeness-bearing filesystem inventory is emitted only when declared-root state changes; ranged reads retain exact identities for incremental indexing, and private bounded state stores its checkpoint. |
| LLM terminal agent | Strong | Pane inventory, bounded screen text, raw input, command spawn, semantic XML/actions, revisions, and change subscriptions support an observe/act loop without an MCP-specific API. |
| PostgreSQL GUI | Strong | Exact container/network grants, process/execution APIs, bounded output, cancellation, redacted exec environment values, and virtualized rendered tables cover administration without placing passwords in argv. Durable credentials still require a dedicated secret provider; private extension state is not a vault. |
| Container/process inspector | Strong | Container inventories, immutable IDs and generations, exact resource selectors, process snapshots, executions, logs, lifecycle controls, and observed wait helpers are present. |
| Single-file workspace editor | Strong | \`[filesystem]\` grants read, write, create, delete, and rename roots independently, so consent to modify one exact file cannot create, remove, or move it. \`stat\` plus \`writeObserved\` provides compare-and-swap replacement. |
| UI inspection/automation | Strong | Native panes expose bounded, redacted semantic XML and revision-bound advertised actions; terminal panes expose bounded screen/history text. Arbitrary pixel/OCR access is intentionally absent. |
| Layout/tab controller | Strong | Topology, pinning, split, focus, ratio, retitle, close, occupant switching, and observed variants cover layout control. |
| Extension catalogue/manager | Partial | Discovery can be rendered from a catalogue owned by the manager extension; acquisition/install/update/enable/disable/remove are complete. The host does not define or trust a global catalogue service. |
| Extension configuration/state | Strong | One authenticated extension-owned blob (1 MiB maximum) survives restart/update and is cleared on successful uninstall. It is private state, not an encrypted secret vault. |

Capabilities use \`group:verb\` wire names. Filesystem authority is additionally
confined by exact consented roots, including a single file. Container authority is
the intersection of a verb capability and separately consented resource selectors.

### Per-resource grants

Manifests declare at most 128 exact selectors under \`[containers]\`, for example
\`selectors = [{ id = "<immutable-id>" }, { name = "database" }]\`. Install and
update calls carry the independently selected \`ContainerGrant\`; the host persists
its intersection with the manifest beside the image digest. Lists, subscriptions,
deep reads, execution access, terminal attachment and lifecycle calls are filtered
or denied at the Rust dispatch boundary. Exact-ID and wildcard mutations operate
on immutable IDs. Name-scoped mutations fail closed until the host can assert the
observed generation atomically with the mutation.

Creation additionally requires \`create = true\`. Visibility never implies create.
Omitting \`[containers]\` means no container authority, even with a container verb
capability. Workspace-wide authority is explicit: \`selectors = [{ all = true }]\`.

Networks use the same two-dimensional model. Manifests request exact network IDs
or names (or explicit \`all\`) under \`[networks]\`; installation consent intersects
that request, and creation is separately consented. Inventory, inspection, removal,
connection, disconnection, and snapshots are filtered or denied against the
persisted selectors.

Filesystem grants implement the same two-dimensional model: \`filesystem:read\` and
\`filesystem:write\` permits the mutation domain, while independently consented write, create, delete, and rename roots decide
the resource. Writable roots are not implicitly readable. Container enforcement follows that
order; the JavaScript client's checks are never treated as a security boundary.

${sections}

## Observe before mutating

Inventory, inspection, pane text, file ranges, executions, pulls, and acquisitions
return the identity/generation/revision fields accepted by revision-bound or
destructive mutations. Keep those exact values through user or agent consent;
do not replace them with names, prefixes, mutable tags, PIDs, or a newer snapshot.
Prefer the revision-bound methods whenever a decision is separated from its mutation.
Process PIDs are snapshot display values and may be reused.

\`\`\`js
const pane = (await host.terminal.panes()).panes[0];
const tree = await host.terminal.semantics(pane.slot);
await host.terminal.act(pane.slot, {
  generation: tree.generation, revision: tree.revision,
  node: tree.root.id, action: 'focus',
});
\`\`\`

## Events

Subscriptions are credit-controlled and bounded. The host sends an initial snapshot,
coalesces latest state while credit is exhausted, and returns credit only after the
client delivers an event. Always unsubscribe or use a \`watch*\` disposer.

${events}

## Protocol bounds

The generated \`PROTOCOL_BOUNDS\` values are:

${bounds}

Collection replies also carry their own \`truncated\`/\`eof\` fields where defined.
Terminal reads are interpreted bounded screen/history snapshots—not raw stdout/stderr.
Container and execution log methods return bounded stdout/stderr byte arrays with
completeness flags. Semantic XML escapes values, redacts sensitive fields, and applies
depth, node, and text bounds.

## Renderer-internal requests

These are intentionally owned by \`@husklet/react\` rather than exposed as ordinary
workspace facade calls:

${internal}
`;

if (process.argv.includes('--write')) fs.writeFileSync(output, reference);
else
  assert.equal(
    fs.readFileSync(output, 'utf8'),
    reference,
    'API.md is stale; run npm run api:generate',
  );

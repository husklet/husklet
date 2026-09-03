import assert from 'node:assert/strict';
import fs from 'node:fs';
import { fileURLToPath } from 'node:url';

import { PROTOCOL_BOUNDS, PROTOCOL_TOPICS, protocolSurface, requestCapability } from '../src/index.js';

const output = fileURLToPath(new URL('../API.md', import.meta.url));
const execution = new Set(['processes', 'execution', 'executions', 'executionLogs', 'waitExecution', 'signalExecution', 'removeExecution']);
const semantic = new Set(['semantics', 'act']);
const groups = new Map([
  ['Workspace', []], ['Containers', []], ['Processes and executions', []], ['Terminal and panes', []],
  ['Files', []], ['Images', []], ['Networks', []], ['Volumes', []], ['Extensions', []], ['Semantics', []],
]);

for (const [wire, route] of Object.entries(protocolSurface.requests)) {
  if (route.kind !== 'facade') continue;
  const [namespace, method] = route.api.includes('.') ? route.api.split('.') : ['workspace', route.api];
  const group = namespace === 'containers' && execution.has(method) ? 'Processes and executions'
    : namespace === 'terminal' && semantic.has(method) ? 'Semantics'
      : ({ workspace: 'Workspace', containers: 'Containers', terminal: 'Terminal and panes', files: 'Files', images: 'Images', networks: 'Networks', volumes: 'Volumes', extensions: 'Extensions' })[namespace];
  assert(group, `no documentation group for ${wire}`);
  groups.get(group).push(`- \`host.${route.api}(...)\` — \`${wire}\`, requires \`${requestCapability(wire)}\`.`);
}
groups.get('Terminal and panes').push(
  '- `host.terminal.toText(...)` — discovers a pane and returns visible terminal screen text or bounded semantic XML; requires `pane-observe` and the corresponding `terminal-output` or `pane-semantic-read` grant.',
  '- `host.terminal.waitForText(...)` — arms pane-change observation, ignores the unchanged cursor, then returns a fresh bounded text projection; requires `pane-observe` and the corresponding read grant.',
  '- `host.terminal.actAndWait(...)` — arms pane observation before a revision-bound semantic action, then returns its changed bounded projection; requires `pane-observe`, `pane-semantic-control`, and the corresponding read grant.',
  '- `host.terminal.switchOccupantAndWait(...)` — arms observation before an observed occupant switch and verifies the exact terminal or extension/provider identity; requires `pane-observe` and `terminal-control`.',
);
groups.get('Extensions').push(
  '- `host.extensions.waitForAcquisition(...)` — waits for an exact acquisition job revision to advance, then reads its authoritative full status; requires `extension-install`.',
  '- `host.extensions.enableAndWait(...)` — arms inventory before enabling an exact installed digest, then verifies its durable enabled state; requires `extension-read` and `extension-control`.',
  '- `host.extensions.disableAndWait(...)` — arms inventory before disabling an exact installed digest, then verifies durable standby; provider withdrawal remains separately observable; requires `extension-read` and `extension-control`.',
  '- `host.extensions.retryAndWait(...)` — arms inventory before retrying an exact faulted digest, rejects replacement/disappearance, then verifies durable duty; requires `extension-read` and `extension-control`.',
  '- `host.extensions.removeAndWait(...)` — arms inventory before removing an exact installed digest, then proves that digest is absent and reports any same-name replacement; requires `extension-read` and `extension-control`.',
  '- `host.extensions.installAndWait(...)` / `updateAndWait(...)` — inspect the exact ready acquisition revision, arm inventory before commit, and verify the returned and published name/digest; requires `extension-install` and `extension-read`.',
  '- `host.containers.startAndWait(...)` — acknowledges bounded inventory before starting an immutable ID, ignores the unchanged initial snapshot, and returns only on a later running state; requires `container-read` and `container-control`.',
);

const topicCapability = Object.fromEntries(PROTOCOL_TOPICS.map(({ wire, capability }) => [wire, capability]));
const sections = [...groups].map(([heading, operations]) => `## ${heading}\n\n${operations.join('\n')}`).join('\n\n');
const events = Object.keys(protocolSurface.topics).map((topic) =>
  `- \`host.subscribe('${topic}')\` / \`host.unsubscribe('${topic}')\` — requires \`${topicCapability[topic]}\`.`).join('\n');
const internal = Object.entries(protocolSurface.requests).filter(([, route]) => route.kind === 'internal')
  .map(([wire, route]) => `- \`${wire}\` — ${route.rationale}.`).join('\n');
const bounds = Object.entries(PROTOCOL_BOUNDS).map(([name, value]) => `- \`${name}\`: ${value}`).join('\n');

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

${sections}

## Observe before mutating

Inventory, inspection, pane text, file ranges, executions, pulls, and acquisitions
return the identity/generation/revision fields accepted by their observed or
destructive counterparts. Keep those exact values through user or agent consent;
do not replace them with names, prefixes, mutable tags, PIDs, or a newer snapshot.
Legacy unobserved pane/file methods remain for compatibility, while the \`Observed\`
methods are the safe default. Process PIDs are snapshot display values and may be reused.

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
else assert.equal(fs.readFileSync(output, 'utf8'), reference, 'API.md is stale; run npm run api:generate');

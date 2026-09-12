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
  ['Extension preferences', []],
  ['Extension credentials', []],
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
            preferences: 'Extension preferences',
            credentials: 'Extension credentials',
            images: 'Images',
            networks: 'Networks',
            volumes: 'Volumes',
            extensions: 'Extensions',
            notifications: 'Notifications',
          }[namespace];
  assert(group, `no documentation group for ${wire}`);
  groups
    .get(group)
    .push(
      wire === 'notification_publish'
        ? '- `host.notifications.publish(...)` — queues a bounded, extension-attributed OS notification; the reply acknowledges host acceptance, not platform delivery; requires `notifications:publish`.'
        : `- \`host.${route.api}(...)\` — \`${wire}\`, requires \`${requestCapability(wire)}\`.`,
    );
}
groups
  .get('Containers')
  .push(
    '- Container list, inspection, and inventory snapshots include a bounded `ports` view, preserving automatically assigned host ports for long-lived service extensions under the same container selector authority.',
    '- Extension sidecars have no network interface. Service clients run through exact-generation `containers.execStreaming(...)` / `execJsonLines(...)`; credentials are resolved into the child environment by the host and never returned to extension code. Use `inspectObserved(...)` for a saved service target so container replacement requires explicit reselection.',
  );
groups
  .get('Terminal and panes')
  .push(
    '- `host.paneChanges(...)` — consumes pane replacement/layout revisions as an async generator; protocol event credit remains withheld until the consumer advances, and abort or disconnect rejects the pending iteration.',
    "- Process-lifetime layout operations are compound authority: opening or splitting a pane, closing a pane, and switching its occupant require both `terminals:layout-control` and `terminals:process-control`, even when the protocol table names the operation's primary capability.",
    '- `host.terminal.toText(...)` — discovers a pane and returns visible terminal screen text or bounded semantic XML; requires `panes:observe` and the corresponding `terminals:output` or `panes:semantic-read` grant.',
    '- `host.terminal.readAll(...)` — discovers panes once and converts each to terminal transcript or bounded semantic XML, reports incomplete discovery, and refuses cursor races; requires `panes:observe`, `terminals:output`, and `panes:semantic-read` for mixed workspaces.',
    '- `host.terminal.readAllStable(...)` — reads every discovered pane and then rechecks the complete ordered inventory, retrying a bounded number of times rather than returning a mixed layout; accepts an `AbortSignal` and throws `PaneInventoryChangedError` with recovery cursors when churn exceeds the attempt bound.',
    '- UI projections returned by `toText`, `readAll`, `readAllStable`, and `inspectAndAct` disclose `complete`, `sourceTruncated`, and `projectionTruncated` separately; `semanticText(tree)` provides the same typed result when converting an already-read tree, while `semanticXml(tree)` remains the string-only shorthand.',
    '- `host.terminal.waitForText(...)` — arms pane-change observation, immediately reconciles state that advanced between agent iterations, then waits for and returns a fresh bounded text projection; accepts an `AbortSignal` for prompt cancellation and requires `panes:observe` plus the corresponding read grant.',
    '- `host.terminal.actAndWait(...)` — arms pane observation before a revision-bound semantic action, then returns its changed bounded projection; requires `panes:observe`, `panes:semantic-control`, and the corresponding read grant.',
    '- `host.terminal.switchOccupantAndWait(...)` — arms observation before an observed occupant switch and verifies the exact terminal or extension/provider identity; requires `panes:observe`, `terminals:layout-control`, and `terminals:process-control`.',
    '- `host.terminal.splitAndWait(...)` — arms pane changes before a generation/revision-bound split and verifies the returned child slot from bounded inventory; requires `panes:observe`, `terminals:layout-control`, and `terminals:process-control`.',
    '- `host.terminal.closeAndWait(...)` — arms pane changes before a generation/revision-bound close and proves absence only from a complete pane inventory; requires `panes:observe`, `terminals:layout-control`, and `terminals:process-control`.',
    '- `host.terminal.retitleAndWait(...)` — arms pane changes before a generation/revision-bound retitle and verifies the exact title at an advanced revision; requires `panes:observe` and `terminals:layout-control`.',
    '- `host.terminal.focusAndWait(...)` — arms pane changes before generation/revision-bound focus and verifies the same pane is focused at an advanced revision; requires `panes:observe` and the least-privilege `terminals:focus` grant, which cannot close or rearrange panes.',
    '- `host.terminal.writeAndWait(...)` — arms and reads the exact terminal screen cursor before writing bounded bytes, then returns a later bounded screen revision; requires `panes:observe`, `terminals:output`, and `terminals:input`.',
    '- `host.terminal.writeObservedAndWait(...)` — the snapshot-bound form of `writeAndWait`: it accepts a previously read `PaneText` directly, rejects stale authority, follows pane-generation replacement, and supports `AbortSignal` cancellation; requires `panes:observe`, `terminals:output`, and `terminals:input`.',
    '- `host.terminal.writeObservedAndWaitForText(...)` — writes against an inspected terminal cursor, then returns either terminal text or bounded semantic XML when the slot is replaced by a surface; requires `panes:observe`, `terminals:output`, `terminals:input`, and semantic-read authority for a surface replacement.',
    '- `host.terminal.spawnAndWait(...)` — arms and reads the exact terminal screen cursor before a generation/revision-bound argv spawn, then returns a later bounded screen revision; requires `panes:observe`, `terminals:output`, and `terminals:process-control`.',
    '- `host.terminal.resizeGridAndWait(...)` — arms and reads the exact terminal screen cursor before a generation/revision-bound resize, then verifies the requested columns and rows on a later screen revision; requires `panes:observe`, `terminals:output`, and `terminals:layout-control`.',
    '- `host.terminal.ratioAndWait(...)` — arms pane observation before a generation/revision-bound ratio change, then verifies the advanced pane and resulting topology (allowing host pixel quantization); requires `panes:observe`, `terminals:read`, and `terminals:layout-control`.',
    '- `host.terminal.openTabAndWait(...)` — arms pane observation before opening the session-owned tab and verifies a pane under the exact returned tab identity; post-creation observation failures retain `{ tab, title }` in `TerminalOperationError`; requires `panes:observe`, `terminals:layout-control`, and `terminals:process-control`.',
  );
groups
  .get('Private extension state')
  .push(
    '- `host.state.readJson(codec)` / `writeJson(observed, value, codec)` — decode and encode the bounded blob through an extension-owned runtime validator/migrator.',
    '- `host.state.updateJson(codec, update, { attempts })` — retries only CAS conflicts (up to 16 attempts); `update` may run more than once and must be safe to repeat.',
    '- `host.credentials` — named per-extension credentials in mode-0600 atomic host files with CAS mutation; reads require an exact 1–64 byte key and no value-listing API exists. Values are limited to 64 KiB, 64 entries, and 4 MiB encoded total. This is access isolation, not encryption or an OS keychain.',
    '- `host.containers.execWithCredentials(...)` — executes with at most 64 unique credential-to-environment bindings validated before framing and resolved by exact credential key inside the host; requires both `containers:execute` and `credentials:inject`, and never returns credential bytes to JavaScript.',
  );
groups
  .get('Files')
  .push(
    '- `host.files.scopeChanges(page, roots)` — filters a change page to configured exact/subtree roots while preserving its global journal cursor, so an indexer checkpoints unrelated permitted changes without indexing them.',
    "- `host.files.changePages(...)` — exposes cursor-safe bounded filesystem change polling as a consumer-driven async generator. Slow consumers issue no next request; abort interrupts idle polling; truncation is yielded explicitly; host and transport failures reject the caller's pending `next()`.",
    '- `host.files.watchChanges(...)` — polls cursor-safe bounded pages and delivers change, truncation, and cursor-only advances so an indexer can durably resume even when consent filtering hides every path in a revision; its stop handle exposes `done` for immediate listener/transport failure supervision; requires `filesystem:read`.',
    '- `host.files.walk(...)` — traverses arbitrarily broad directory trees through identity-pinned bounded pages with consumer backpressure and memory proportional to active depth; rejects duplicate or backward host cursors; requires `filesystem:read`.',
    '- `host.files.readRanges(...)` — reads up to 64 separately confined ranges in one 64 KiB aggregate request and rejects different identities or totals for repeated paths, so one batch cannot mix file generations; requires `filesystem:read`.',
    "- `host.files.write(...)`, `writeObserved(...)`, and `createObserved(...)` — consume byte iterables only through the host's 64 KiB mutation bound before framing; observed variants retain inode identity/race protection.",
    '- `host.files.readChunks(...)` — iterates an identity-pinned file through bounded ranges with consumer backpressure; requires `filesystem:read`.',
    '- `host.files.readText(path, { maxBytes, ... })` — reads identity-pinned UTF-8 across range boundaries, reports malformed text as `FileTextDecodeError`, and reports an oversized generation as `FileTextLimitError` with exact path, identity, authoritative total, and caller limit; requires `filesystem:read`.',
  );
groups
  .get('Extension preferences')
  .push(
    '- Preferences are workspace-local and host-namespaced to the authenticated extension. Keys are 1–64 restricted ASCII bytes, strings are at most 1024 UTF-8 bytes, numbers are JavaScript-safe integers, and each extension may hold at most 64 entries. Arrays, objects, null, and unbounded JSON are not accepted.',
  );
groups
  .get('Terminal and panes')
  .push(
    '- A supervised terminal command remains inspectable, readable, writable, waitable, and cancellable by its immutable returned identity after its originating pane is replaced or the extension reconnects. The pane snapshot fences creation only.',
    '- Every supervised command carries its authenticated, immutable installation incarnation. All follow-up calls echo that owner and the Rust host rejects copied or lifecycle-stale command authority before execution lookup. Reconnecting the same installed record remains valid; disable, update, regrant, uninstall, and reinstall rotate or discard the incarnation.',
    '- `host.terminal.commandText(...)` interrupts idle polling immediately on abort, cancels the exact owned command, and throws `TerminalCommandOperationError` after any post-start failure with the immutable command, failed phase, and last completely consumed output cursor.',
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
    '- `host.extensions.cancelAcquisitionAndWait(job, revision, options)` — subscribes before cancellation, rechecks the exact revision, and returns only after authoritative `cancelled` status or an explicit timeout; `options.signal` aborts observation and releases the subscription; requires `extensions:install`.',
    '- `host.extensions.enableAndWait(...)` — arms inventory before enabling an exact installed digest, then verifies its durable enabled state; requires `extensions:read` and `extensions:control`.',
    '- `host.extensions.disableAndWait(...)` — arms inventory before disabling an exact installed digest, then verifies durable standby; provider withdrawal remains separately observable; requires `extensions:read` and `extensions:control`.',
    '- `host.extensions.retryAndWait(...)` — arms inventory before retrying an exact faulted digest, rejects replacement/disappearance, then verifies durable duty; requires `extensions:read` and `extensions:control`.',
    '- `host.extensions.removeAndWait(...)` — arms inventory before removing an exact installed digest, then proves that digest is absent and reports any same-name replacement; requires `extensions:read` and `extensions:remove`.',
    '- `host.extensions.installAndWait(job, revision, review)` / `updateAndWait(job, revision, review)` — accept all reviewed capabilities and resource grants in one named object, inspect the exact ready acquisition revision, send its immutable digest as commit CAS authority, arm inventory before commit, and verify the returned and published name/digest; requires `extensions:install` and `extensions:read`.',
    '- `host.containers.startAndWait(...)` — acknowledges bounded inventory before starting an immutable ID, ignores the unchanged initial snapshot, and returns only on a later running state; requires `containers:read` and `containers:lifecycle`.',
    '- `host.containers.stopAndWait(...)` — acknowledges bounded inventory before stopping an immutable ID, ignores unchanged/running snapshots, and returns only on a later exited state; requires `containers:read` and `containers:lifecycle`.',
    '- `host.containers.removeAndWait(...)` — arms an explicit completeness-bearing inventory before removal and accepts absence only from a later `complete: true` snapshot; requires `containers:read` and `containers:remove`.',
    '- `host.containers.restartAndWait(...)` — arms inventory before restarting an immutable ID and accepts only `running` at a generation newer than the caller observed; requires `containers:read` and `containers:lifecycle`.',
  );
groups
  .get('Processes and executions')
  .push(
    '- `host.containers.execAndWait(id, options)` — prevalidates bounded execution/output options, executes by immutable container ID, waits, then fetches bounded logs; failures retain the execution ID, and log-phase failures retain the authoritative completed summary, in `ExecutionOperationError`; records are never auto-removed.',
    '- `host.containers.execStreaming(id, generation, options, onPage)` — executes by immutable container ID, uses ordinary execute authority when credential bindings are empty, and otherwise resolves credential keys directly into process environment variables; it reports the live execution ID through an awaited `options.onStarted` hook, concurrently writes bounded `options.input` chunks and drains bounded output pages so neither pipe can deadlock the other, preserves backpressure and explicit EOF in both directions, interrupts stalled callbacks or input iterators on abort, releases an idle dynamic input source when the helper reaches EOF, cancels on abort or callback failure, refuses and cancels an execution that still reports running after output EOF, and returns only a completed summary without auto-removing the record.',
    '- `host.containers.executionOutputPages(...)` — rejects pages above 256 KiB, gaps, backward or stalled cursors, contradictory EOF/continuation flags, and entry sequences that do not match their cursor before another page can be requested. Aborting between requests keeps the session; aborting an in-flight request closes the ambiguous ordered connection so the extension can reconnect and resume from its last acknowledged cursor.',
    '- `ExecutionOperationError.after` — carries the last output sequence acknowledged by `execStreaming` or `resumeExecutionStreaming`; reconnecting test runners can replay from that cursor without duplicating committed results. Input failures are reported with the typed `input` phase.',
    '- `host.containers.execLines(id, generation, options, onLine)` — streams bounded UTF-8 stdout records with serial callback backpressure, credential references, cancellation, and incremental stderr delivery; an unterminated final record and final stderr decoder tail remain AbortSignal-cancellable after process EOF, with the completed execution retained on failure.',
    '- `host.containers.execText(id, generation, options)` — executes with the same optional credential references and incrementally decodes stdout/stderr under a required aggregate `maxBytes` bound; overflow or malformed UTF-8, including an incomplete final sequence discovered at EOF, preserves its ID and completed summary in `ExecutionOperationError`.',
    '- `host.containers.execJsonLines(id, generation, options, onValue)` — streams bounded newline-delimited JSON values with serial callback backpressure; supports the same live `onStarted` identity hook, credential references, cancellation, and stderr handling.',
    '- `host.containers.writeExecutionStdin(id, input)` — writes one 1–65536-byte chunk under the separate `containers:input` capability and acknowledges only after transport backpressure.',
    '- `host.containers.closeExecutionStdin(id)` — explicitly half-closes stdin while durable output remains readable; disconnect does not substitute for EOF.',
    '- `host.containers.pipeExecutionStdin(id, source, options)` — consumes bounded chunks serially and optionally closes on successful exhaustion; abort stops production without implicitly closing stdin.',
    '- `host.containers.signalExecutionAndWait(id, signal, after, options)` — arms execution observation, verifies the immutable execution cursor, signals, then awaits an explicit changed or exited state; requires `containers:read` and `containers:execute`.',
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
| Code/embedding index | Strong | \`files.pathGrant\` distinguishes exact-file roots from recursive subtree roots with host-compatible component matching before any probe; bounded completeness-bearing inventory, identity-preserving ranged reads, and private bounded state support incremental checkpoints. The grant result is a planning snapshot and every operation remains host-authorized. |
| LLM terminal agent | Strong | Pane inventory, bounded screen text, raw input, semantic XML/actions, and a supervised command lifecycle support an observe/act loop without an MCP-specific API. Commands are fenced to an exact pane snapshot, carry immutable identities, bounded cursor output, explicit stdin receipts, cancellation, EOF, and authoritative exit status without parsing prompts. |
| PostgreSQL GUI | Strong | Exact container/network grants, process/execution APIs, bounded stdin/output, explicit EOF, cancellation, host-resolved credential references, published-port discovery, and virtualized rendered tables cover administration without placing passwords in argv. Credentials have host-private file isolation, not OS-keychain encryption; host port forwarding remains absent. |
| Container/process inspector | Strong | Container inventories, immutable IDs and generations, exact resource selectors exposed through caller-only \`grantedContainers\`, \`grantedImages\`, \`grantedNetworks\`, and \`grantedVolumes\`, process snapshots, executions, logs, lifecycle controls, and observed wait helpers are present. |
| Single-file workspace editor | Strong | \`[filesystem]\` grants read, write, create, delete, and rename roots independently, and \`grantedFilesystem\` exposes only the caller's immutable effective selectors, so consent to modify one exact file cannot create, remove, or move it. \`stat\` plus \`writeObserved\` provides compare-and-swap replacement. |
| Atomic multi-file editor | Unsupported | Individual observed writes preflight and publish one file at a time. POSIX has no atomic rename transaction spanning several names or directories, so a later stale identity or I/O failure can leave earlier files committed. No batch API is exposed until the host has a recoverable workspace transaction layer. |
| UI inspection/automation | Strong | Native panes expose bounded, redacted semantic XML and revision-bound advertised actions; terminal panes expose bounded screen/history text. Arbitrary pixel/OCR access is intentionally absent. |
| Layout/tab controller | Strong | Topology, pinning, split, focus, ratio, retitle, close, occupant switching, and observed variants cover layout control. |
| Extension catalogue/manager | Partial | Discovery can be rendered from the host's bounded catalogue and \`requireCompleteCatalogue\` fails closed rather than treating truncation as a searchable store. Acquisition/install/update/enable/disable/remove are complete. There is no snapshot-pinned catalogue continuation or verified global publisher trust service. |
| Extension configuration/state | Strong | One authenticated extension-owned blob (1 MiB maximum) survives restart/update and is cleared on successful uninstall. \`grantedWorkspaceEnvironment\` exposes the caller's immutable exact environment scope. It is private state, not an encrypted secret vault. |

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

import { Buffer } from 'node:buffer';

const text = (answer, tool) => {
  const value = answer?.content?.find(({ type }) => type === 'text')?.text;
  if (answer?.isError || typeof value !== 'string') throw new Error(`${tool} failed: ${value ?? 'no text result'}`);
  return value;
};

const call = async (client, name, args = {}) => text(
  await client.callTool({ name, arguments: args }), name,
);

const attribute = (xml, name) => {
  const match = xml.match(new RegExp(`(?:<husklet-pane|<pane|<node)[^>]*\\b${name}="(\\d+)"`));
  if (!match) throw new Error(`semantic snapshot has no numeric ${name}`);
  return Number(match[1]);
};

const nodeForLabel = (xml, label) => {
  const encoded = label.replaceAll('&', '&amp;').replaceAll('<', '&lt;').replaceAll('>', '&gt;')
    .replaceAll('"', '&quot;').replaceAll("'", '&apos;');
  const escaped = encoded.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  const match = xml.match(new RegExp(`<node\\s+id="(\\d+)"[^>]*>\\s*<label>${escaped}</label>`));
  if (!match) throw new Error(`semantic snapshot has no ${JSON.stringify(label)} action`);
  return Number(match[1]);
};

/**
 * One bounded agent turn over the public MCP tool surface.
 *
 * The caller supplies deliberate bytes and a semantic label selected by the
 * model. This helper never retries or sleeps: it consumes one host-backed pane
 * notification, then refreshes the changed snapshot once.
 */
export async function runPaneAgentTurn(client, {
  terminalBytes = Uint8Array.from([0x03]),
  actionLabel = 'Refresh',
  terminalSlot,
  semanticSlot,
  waitMs = 5_000,
} = {}) {
  if (!(terminalBytes instanceof Uint8Array) || terminalBytes.byteLength < 1 || terminalBytes.byteLength > 65_536) {
    throw new TypeError('terminalBytes must be a Uint8Array of 1..65536 bytes');
  }

  const inventory = JSON.parse(await call(client, 'husklet_pane_list'));
  const panes = Array.isArray(inventory.panes) ? inventory.panes : [];
  for (const [name, slot] of [['terminalSlot', terminalSlot], ['semanticSlot', semanticSlot]]) {
    if (slot !== undefined && (typeof slot !== 'string' || slot.length < 1 || slot.length > 256)) {
      throw new TypeError(`${name} must be a nonempty string of at most 256 characters`);
    }
  }
  const terminal = panes.find((pane) => pane.kind === 'terminal' && (terminalSlot === undefined || pane.slot === terminalSlot));
  const semantic = panes.find((pane) => (pane.kind === 'native' || pane.kind === 'surface')
    && (semanticSlot === undefined || pane.slot === semanticSlot));
  if (!terminal || !semantic) throw new Error('one terminal and one semantic pane are required');

  const terminalBefore = await call(client, 'husklet_pane_read', { slot: terminal.slot, lines: 100 });
  const terminalGeneration = attribute(terminalBefore, 'generation');
  const terminalRevision = attribute(terminalBefore, 'revision');
  const semanticBefore = await call(client, 'husklet_pane_snapshot', { slot: semantic.slot });
  const generation = attribute(semanticBefore, 'generation');
  const revision = attribute(semanticBefore, 'revision');
  if (!Number.isSafeInteger(generation) || generation < 0
      || !Number.isSafeInteger(revision) || revision < 0) {
    throw new Error('semantic pane discovery and snapshot must expose a nonnegative generation/revision cursor');
  }
  const node = nodeForLabel(semanticBefore, actionLabel);

  const terminalMutation = JSON.parse(await call(client, 'husklet_terminal_write_bytes_wait', {
    slot: terminal.slot, generation: terminalGeneration, revision: terminalRevision,
    input_base64: Buffer.from(terminalBytes).toString('base64'), timeout_ms: waitMs,
  }));
  const terminalChanged = terminalMutation.observation;
  const terminalAfter = terminalChanged.changed
    ? await call(client, 'husklet_pane_read', { slot: terminal.slot, lines: 100 })
    : null;
  // Arm the one-shot subscription first; request ordering prevents a fast UI
  // update from racing past observation.
  const semanticMutation = JSON.parse(await call(client, 'husklet_pane_action_wait', {
    slot: semantic.slot, generation, revision, node, action: 'invoke', timeout_ms: waitMs,
  }));
  const changed = semanticMutation.observation;
  const semanticAfter = changed.changed
    ? await call(client, 'husklet_pane_snapshot', { slot: semantic.slot })
    : null;

  return {
    terminal: { slot: terminal.slot, generation: terminalGeneration, revision: terminalRevision,
      before: terminalBefore, changed: terminalChanged, after: terminalAfter },
    semantic: { slot: semantic.slot, generation, revision, node, before: semanticBefore, changed, after: semanticAfter },
  };
}

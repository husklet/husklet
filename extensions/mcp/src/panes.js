import { z } from 'zod';
import { result } from './bounds.js';

const XML_LIMIT = 64 * 1024;
const NODE_LIMIT = 256;
const DEPTH_LIMIT = 32;
const TEXT_LIMIT = 256;
const SECRET = /(password|secret|token|credential|private.?key)/i;
const bytes = (text) => new TextEncoder().encode(text).byteLength;
const boundedText = (value, limit = TEXT_LIMIT) => {
  const characters = Array.from(String(value));
  return { value: characters.slice(0, limit).join(''), truncated: characters.length > limit };
};
const escape = (value) => Array.from(String(value), (character) => {
  const point = character.codePointAt(0);
  return point <= 0x1f && point !== 0x09 && point !== 0x0a && point !== 0x0d
    || (point >= 0x7f && point <= 0x9f) || (point >= 0xd800 && point <= 0xdfff) ? '\uFFFD' : character;
}).join('')
  .replaceAll('&', '&amp;').replaceAll('<', '&lt;').replaceAll('>', '&gt;')
  .replaceAll('"', '&quot;').replaceAll("'", '&apos;')
  .replaceAll('\t', '&#x9;').replaceAll('\n', '&#xA;').replaceAll('\r', '&#xD;');

/** Deterministic, bounded XML-like text from the host's typed semantic tree. */
export function semanticXml(tree) {
  if (!Number.isSafeInteger(tree?.generation) || tree.generation < 0
    || !Number.isSafeInteger(tree?.revision) || tree.revision < 0) {
    throw new TypeError('semantic tree requires nonnegative safe integer generation and revision');
  }
  let output = '';
  let used = 0;
  let nodes = 0;
  let cut = false;
  const append = (text, reserve = 0) => {
    const size = bytes(text);
    if (used + size + reserve > XML_LIMIT) { cut = true; return false; }
    output += text; used += size; return true;
  };
  const attr = (value) => escape(boundedText(value).value);
  const node = (entry, depth, reserve) => {
    if (!entry || typeof entry !== 'object' || nodes >= NODE_LIMIT || depth >= DEPTH_LIMIT) { cut = true; return; }
    nodes += 1;
    const actionValues = Array.isArray(entry.actions) ? entry.actions : [];
    const actions = actionValues.slice(0, 16).map(attr).join(',');
    const id = boundedText(entry.id ?? ''); const role = boundedText(entry.role ?? '');
    const close = '</node>';
    if (!append(`<node id="${escape(id.value)}"${id.truncated ? ' id-truncated="true"' : ''} role="${escape(role.value)}"${role.truncated ? ' role-truncated="true"' : ''} disabled="${entry.disabled === true}" destructive="${entry.destructive === true}" actions="${actions}"${actionValues.length > 16 ? ' actions-truncated="true"' : ''}>`, reserve + bytes(close) + 14)) return;
    if (entry.label != null) { const label = boundedText(entry.label); append(`<label${label.truncated ? ' truncated="true"' : ''}>${escape(label.value)}</label>`, reserve + bytes(close) + 14); }
    if (entry.value != null) {
      const value = SECRET.test(`${entry.role ?? ''} ${entry.label ?? ''}`) ? '[redacted]' : entry.value;
      const field = boundedText(value);
      append(`<value${field.truncated ? ' truncated="true"' : ''}>${escape(field.value)}</value>`, reserve + bytes(close) + 14);
    }
    for (const child of Array.isArray(entry.children) ? entry.children : []) {
      if (cut) break;
      node(child, depth + 1, reserve + bytes(close));
    }
    if (cut) append('<truncated/>', reserve + bytes(close));
    append(close);
  };
  const paneClose = '</pane>';
  append(`<pane slot="${attr(tree?.slot ?? '')}" generation="${attr(tree.generation)}" revision="${attr(tree.revision)}" truncated="${tree?.truncated === true}">`, bytes(paneClose) + 14);
  node(tree?.root, 0, bytes(paneClose));
  if (cut) append('<truncated/>', bytes(paneClose));
  append(paneClose);
  return output;
}

const paneSchema = z.object({
  slot: z.string().min(1).max(256),
  lines: z.number().int().min(1).max(500).default(200),
}).strict();

function leaves(topology) {
  const found = [];
  const walk = (node, tab) => {
    if (!node || typeof node !== 'object') return;
    if (node.kind === 'pane' && node.pane) found.push({ ...node, tab });
    if (node.kind === 'split') { walk(node.first, tab); walk(node.second, tab); }
  };
  for (const tab of Array.isArray(topology?.tabs) ? topology.tabs : []) walk(tab.root, tab);
  return found;
}

const metadata = (value) => value == null ? '' : escape(SECRET.test(String(value)) ? '[redacted]' : String(value).slice(0, TEXT_LIMIT));

/** One bounded XML document for either terminal text or a semantic surface. */
export async function paneXml(terminal, slot, lines = 200) {
  const inventory = await terminal.panes();
  const descriptor = Array.isArray(inventory?.panes)
    ? inventory.panes.find((pane) => pane?.slot === slot) : undefined;
  if (!descriptor) throw new Error(`pane ${JSON.stringify(slot)} is absent from pane inventory`);
  const occupant = descriptor.kind;
  const open = `<husklet-pane slot="${escape(slot)}" occupant="${escape(occupant)}" generation="${escape(descriptor.generation ?? 0)}" revision="${escape(descriptor.revision ?? 0)}">`;
  const close = '</husklet-pane>';
  if (occupant === 'surface' || occupant === 'native') {
    const snapshot = await terminal.semantics(slot);
    if (snapshot?.generation !== descriptor.generation || snapshot?.revision !== descriptor.revision) {
      throw new Error(`pane ${JSON.stringify(slot)} changed while it was being read`);
    }
    const semantic = semanticXml(snapshot);
    if (bytes(open) + bytes(semantic) + bytes(close) <= XML_LIMIT) return `${open}${semantic}${close}`;
    return `${open}<truncated/></husklet-pane>`;
  }
  if (occupant !== 'terminal') throw new TypeError(`pane ${JSON.stringify(slot)} has unsupported occupant ${JSON.stringify(occupant)}`);
  const topology = await terminal.topology();
  const leaf = leaves(topology).find(({ pane }) => pane.slot === slot);
  if (!leaf) throw new Error(`terminal pane ${JSON.stringify(slot)} is inventoried but absent from terminal topology`);
  const screen = await terminal.read(slot, lines);
  if (screen?.generation !== descriptor.generation || screen?.revision !== descriptor.revision) {
    throw new Error(`pane ${JSON.stringify(slot)} changed while it was being read`);
  }
  let output = open;
  let used = bytes(open);
  let cut = false;
  const append = (fragment, reserve = bytes(close)) => {
    const size = bytes(fragment);
    if (used + size + reserve > XML_LIMIT) { cut = true; return false; }
    output += fragment; used += size; return true;
  };
  const active = topology?.active_tab === leaf.tab?.id;
  append(`<terminal tab="${escape(leaf.tab?.id ?? '')}" title="${escape(String(leaf.tab?.title ?? '').slice(0, TEXT_LIMIT))}" active="${active}" focused="${leaf.focused === true}" columns="${escape(screen?.columns ?? '')}" rows="${escape(screen?.rows ?? '')}" cursor-column="${escape(screen?.cursor_column ?? '')}" cursor-row="${escape(screen?.cursor_row ?? '')}" cwd="${metadata(leaf.pane.working_directory)}" command="${metadata(leaf.pane.command)}" truncated="${screen?.truncated === true}">`, bytes('</terminal>') + bytes(close));
  let index = 0;
  for (const line of Array.isArray(screen?.lines) ? screen.lines.slice(0, 500) : []) {
    const field = boundedText(line, 4096);
    if (!append(`<line index="${index}"${field.truncated ? ' truncated="true"' : ''}>${escape(field.value)}</line>`, bytes('</terminal>') + bytes(close) + 14)) break;
    index += 1;
  }
  if (cut || (screen?.lines?.length ?? 0) > 500) append('<truncated/>', bytes('</terminal>') + bytes(close));
  append('</terminal>', bytes(close));
  append(close, 0);
  return output;
}

const xmlResult = (tree) => ({ content: [{ type: 'text', text: semanticXml(tree) }] });

export const semanticAction = z.object({
  slot: z.string().min(1).max(256),
  generation: z.number().int().nonnegative(),
  revision: z.number().int().nonnegative(),
  node: z.number().int().nonnegative(),
  action: z.enum(['invoke', 'change', 'submit', 'toggle', 'expand', 'focus']),
  value: z.string().max(8192).nullable().optional(),
  confirm: z.boolean().optional(),
}).strict();

const findNode = (node, id) => {
  if (!node || typeof node !== 'object') return undefined;
  if (node.id === id) return node;
  for (const child of Array.isArray(node.children) ? node.children : []) {
    const found = findNode(child, id);
    if (found) return found;
  }
  return undefined;
};

export async function observePaneMutation(watchPaneChanges, { slot, generation, revision, timeout }, mutate) {
  let resolveChange;
  const changed = new Promise((resolve) => { resolveChange = resolve; });
  let settled = false;
  const dispose = await watchPaneChanges((change) => {
    const newer = change.generation > generation
      || (change.generation === generation && change.revision > revision);
    if (!settled && change.slot === slot && newer) {
      settled = true;
      resolveChange({ changed: true, change });
    }
  });
  const timer = setTimeout(() => {
    if (!settled) { settled = true; resolveChange({ changed: false }); }
  }, timeout);
  try {
    const mutation = await mutate();
    return { mutation, observation: await changed };
  } finally {
    settled = true;
    clearTimeout(timer);
    await dispose();
  }
}

/** Arm first, retain only bounded invalidations, then bind the wait to the mutation result. */
export async function observePaneMutationResult(watchPaneChanges, cursor, mutate, target) {
  const pending = [];
  let dropped = 0;
  let resolveChange;
  const changed = new Promise((resolve) => { resolveChange = resolve; });
  let settled = false;
  let wanted;
  const accept = (change) => {
    if (!wanted || change.slot !== wanted.slot) return false;
    return wanted.generation == null
      || change.generation > wanted.generation
      || (change.generation === wanted.generation && change.revision > wanted.revision);
  };
  const dispose = await watchPaneChanges((change) => {
    if (settled) return;
    if (wanted && accept(change)) { settled = true; resolveChange({ changed: true, change, dropped }); return; }
    if (!wanted) {
      if (pending.length === 64) { pending.shift(); dropped += 1; }
      pending.push(change);
    }
  });
  const timer = setTimeout(() => {
    if (!settled) { settled = true; resolveChange({ changed: false, dropped }); }
  }, cursor.timeout);
  try {
    const mutation = await mutate();
    wanted = target(mutation, cursor);
    const buffered = pending.find(accept);
    if (buffered) { settled = true; resolveChange({ changed: true, change: buffered, dropped }); }
    return { mutation, observation: await changed };
  } finally {
    settled = true;
    clearTimeout(timer);
    await dispose();
  }
}

export async function performSemanticAction(terminal, { slot, confirm, ...action }) {
  const tree = await terminal.semantics(slot);
  const node = findNode(tree.root, action.node);
  if (!node) throw new Error(`semantic node ${action.node} is absent from revision ${tree.revision}`);
  if (tree.generation !== action.generation) {
    throw new Error(`stale pane generation ${action.generation}; current is ${tree.generation}`);
  }
  if (tree.revision !== action.revision) {
    throw new Error(`stale semantic revision ${action.revision}; current is ${tree.revision}`);
  }
  if (node.disabled === true) throw new Error(`semantic node ${action.node} is disabled`);
  if (!Array.isArray(node.actions) || !node.actions.includes(action.action)) {
    throw new Error(`semantic node ${action.node} does not advertise ${action.action}`);
  }
  if (node?.destructive === true && confirm !== true) throw new Error('destructive pane action requires confirm: true');
  await terminal.act(slot, action);
  return { done: true };
}

export function paneTools(terminal, watchPaneChanges) {
  if (typeof terminal?.semantics !== 'function' || typeof terminal?.act !== 'function') return [];
  const tools = [
    ['husklet_pane_read', 'Read any pane as one bounded occupant-aware XML document.', paneSchema,
      async ({ slot, lines }) => ({ content: [{ type: 'text', text: await paneXml(terminal, slot, lines) }] }), true],
    ['husklet_pane_snapshot', 'Read the bounded semantic tree exposed by a pane.', z.object({ slot: z.string().min(1).max(256) }).strict(),
      async ({ slot }) => xmlResult(await terminal.semantics(slot)), true],
    ['husklet_pane_action', 'Act on a semantic node from a matching pane generation and tree revision.', semanticAction,
      (action) => performSemanticAction(terminal, action)],
  ];
  if (typeof watchPaneChanges === 'function') tools.push([
    'husklet_pane_action_wait',
    'Atomically arm pane observation, perform one revision-fenced semantic action, and return the matching change.',
    semanticAction.extend({ timeout_ms: z.number().int().min(1).max(30_000).default(30_000) }).strict(),
    ({ timeout_ms: timeout, ...action }) => observePaneMutation(
      watchPaneChanges,
      { slot: action.slot, generation: action.generation, revision: action.revision, timeout },
      () => performSemanticAction(terminal, action),
    ),
  ]);
  return tools.map(([name, description, inputSchema, run, formatted = false]) => ({
    name, description, inputSchema,
    run: async (input) => formatted ? run(input) : result(await run(input)),
  }));
}

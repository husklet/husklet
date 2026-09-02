import { z } from 'zod';
import { result } from './bounds.js';

const XML_LIMIT = 64 * 1024;
const NODE_LIMIT = 256;
const DEPTH_LIMIT = 32;
const TEXT_LIMIT = 256;
const SECRET = /(password|secret|token|credential|private.?key)/i;
const bytes = (text) => new TextEncoder().encode(text).byteLength;
const escape = (value) => String(value)
  .replace(/[\u0000-\u0008\u000B\u000C\u000E-\u001F\u007F-\u009F\uD800-\uDFFF]/g, '\uFFFD')
  .replaceAll('&', '&amp;').replaceAll('<', '&lt;').replaceAll('>', '&gt;')
  .replaceAll('"', '&quot;').replaceAll("'", '&apos;')
  .replaceAll('\t', '&#x9;').replaceAll('\n', '&#xA;').replaceAll('\r', '&#xD;');

/** Deterministic, bounded XML-like text from the host's typed semantic tree. */
export function semanticXml(tree) {
  let output = '';
  let used = 0;
  let nodes = 0;
  let cut = false;
  const append = (text, reserve = 0) => {
    const size = bytes(text);
    if (used + size + reserve > XML_LIMIT) { cut = true; return false; }
    output += text; used += size; return true;
  };
  const attr = (value) => escape(String(value).slice(0, TEXT_LIMIT));
const text = (value) => escape(String(value).slice(0, TEXT_LIMIT));
  const node = (entry, depth, reserve) => {
    if (!entry || typeof entry !== 'object' || nodes >= NODE_LIMIT || depth >= DEPTH_LIMIT) { cut = true; return; }
    nodes += 1;
    const actions = Array.isArray(entry.actions) ? entry.actions.slice(0, 16).map(attr).join(',') : '';
    const close = '</node>';
    if (!append(`<node id="${attr(entry.id ?? '')}" role="${attr(entry.role ?? '')}" disabled="${entry.disabled === true}" destructive="${entry.destructive === true}" actions="${actions}">`, reserve + bytes(close) + 14)) return;
    if (entry.label != null) append(`<label>${text(entry.label)}</label>`, reserve + bytes(close) + 14);
    if (entry.value != null) {
      const value = SECRET.test(`${entry.role ?? ''} ${entry.label ?? ''}`) ? '[redacted]' : entry.value;
      append(`<value>${text(value)}</value>`, reserve + bytes(close) + 14);
    }
    for (const child of Array.isArray(entry.children) ? entry.children : []) {
      if (cut) break;
      node(child, depth + 1, reserve + bytes(close));
    }
    if (cut) append('<truncated/>', reserve + bytes(close));
    append(close);
  };
  const paneClose = '</pane>';
  append(`<pane slot="${attr(tree?.slot ?? '')}" revision="${attr(tree?.revision ?? 0)}" truncated="${tree?.truncated === true}">`, bytes(paneClose) + 14);
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
  const topology = await terminal.topology();
  const leaf = leaves(topology).find(({ pane }) => pane.slot === slot);
  if (!leaf) {
    if (slot !== 'workspace') {
      throw new Error(`pane ${JSON.stringify(slot)} is absent from terminal topology`);
    }
    try {
      const semantic = semanticXml(await terminal.semantics(slot));
      const open = `<husklet-pane slot="${escape(slot)}" occupant="native">`;
      const close = '</husklet-pane>';
      if (bytes(open) + bytes(semantic) + bytes(close) <= XML_LIMIT) return `${open}${semantic}${close}`;
      return `${open}<truncated/></husklet-pane>`;
    } catch (cause) {
      throw new Error(`pane ${JSON.stringify(slot)} is absent from topology and exposes no native semantics: ${cause?.message ?? cause}`, { cause });
    }
  }
  const occupant = leaf.pane.occupant;
  const open = `<husklet-pane slot="${escape(slot)}" occupant="${escape(occupant)}">`;
  const close = '</husklet-pane>';
  if (occupant === 'surface') {
    const semantic = semanticXml(await terminal.semantics(slot));
    if (bytes(open) + bytes(semantic) + bytes(close) <= XML_LIMIT) return `${open}${semantic}${close}`;
    return `${open}<truncated/></husklet-pane>`;
  }
  if (occupant !== 'terminal') throw new TypeError(`pane ${JSON.stringify(slot)} has unsupported occupant ${JSON.stringify(occupant)}`);
  const screen = await terminal.read(slot, lines);
  let output = open;
  let used = bytes(open);
  let cut = false;
  const append = (fragment, reserve = bytes(close)) => {
    const size = bytes(fragment);
    if (used + size + reserve > XML_LIMIT) { cut = true; return false; }
    output += fragment; used += size; return true;
  };
  const active = topology?.active_tab === leaf.tab?.id;
  append(`<terminal tab="${escape(leaf.tab?.id ?? '')}" title="${escape(String(leaf.tab?.title ?? '').slice(0, TEXT_LIMIT))}" active="${active}" focused="${leaf.focused === true}" columns="${escape(leaf.grid?.columns ?? '')}" rows="${escape(leaf.grid?.rows ?? '')}" cwd="${metadata(leaf.pane.working_directory)}" command="${metadata(leaf.pane.command)}" truncated="${screen?.truncated === true}">`, bytes('</terminal>') + bytes(close));
  let index = 0;
  for (const line of Array.isArray(screen?.lines) ? screen.lines.slice(0, 500) : []) {
    if (!append(`<line index="${index}">${escape(String(line).slice(0, 4096))}</line>`, bytes('</terminal>') + bytes(close) + 14)) break;
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

export function paneTools(terminal) {
  if (typeof terminal?.semantics !== 'function' || typeof terminal?.act !== 'function') return [];
  const tools = [
    ['husklet_pane_read', 'Read any pane as one bounded occupant-aware XML document.', paneSchema,
      async ({ slot, lines }) => ({ content: [{ type: 'text', text: await paneXml(terminal, slot, lines) }] }), true],
    ['husklet_pane_snapshot', 'Read the bounded semantic tree exposed by a pane.', z.object({ slot: z.string().min(1).max(256) }).strict(),
      async ({ slot }) => xmlResult(await terminal.semantics(slot)), true],
    ['husklet_pane_action', 'Act on a semantic node from a matching tree revision.', semanticAction,
      async ({ slot, confirm, ...action }) => {
        const tree = await terminal.semantics(slot);
        const node = findNode(tree.root, action.node);
        if (!node) throw new Error(`semantic node ${action.node} is absent from revision ${tree.revision}`);
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
      }],
  ];
  return tools.map(([name, description, inputSchema, run, formatted = false]) => ({
    name, description, inputSchema,
    run: async (input) => formatted ? run(input) : result(await run(input)),
  }));
}

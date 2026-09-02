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
    if (!append(`<node id="${attr(entry.id ?? '')}" role="${attr(entry.role ?? '')}" disabled="${entry.disabled === true}" actions="${actions}">`, reserve + bytes(close) + 14)) return;
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

const xmlResult = (tree) => ({ content: [{ type: 'text', text: semanticXml(tree) }] });

export const semanticAction = z.object({
  slot: z.string().min(1).max(256),
  revision: z.number().int().nonnegative(),
  node: z.number().int().nonnegative(),
  action: z.enum(['invoke', 'change', 'submit', 'toggle', 'expand', 'focus']),
  value: z.string().max(8192).nullable().optional(),
}).strict();

export function paneTools(terminal) {
  if (typeof terminal?.semantics !== 'function' || typeof terminal?.act !== 'function') return [];
  return [
    ['husklet_pane_snapshot', 'Read the bounded semantic tree exposed by a pane.', z.object({ slot: z.string().min(1).max(256) }).strict(),
      async ({ slot }) => xmlResult(await terminal.semantics(slot)), true],
    ['husklet_pane_action', 'Act on a semantic node from a matching tree revision.', semanticAction,
      async ({ slot, ...action }) => { await terminal.act(slot, action); return { done: true }; }],
  ].map(([name, description, inputSchema, run, formatted = false]) => ({
    name, description, inputSchema,
    run: async (input) => formatted ? run(input) : result(await run(input)),
  }));
}

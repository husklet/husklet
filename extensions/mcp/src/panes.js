import { z } from 'zod';
import { result } from './bounds.js';

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
      ({ slot }) => terminal.semantics(slot)],
    ['husklet_pane_action', 'Act on a semantic node from a matching tree revision.', semanticAction,
      async ({ slot, ...action }) => { await terminal.act(slot, action); return { done: true }; }],
  ].map(([name, description, inputSchema, run]) => ({ name, description, inputSchema, run: async (input) => result(await run(input)) }));
}

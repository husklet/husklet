import { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js';
import { z } from 'zod';
import { workspace } from '@husklet/react';
import { result } from './bounds.js';
import { paneTools } from './panes.js';
export { paneXml, semanticXml } from './panes.js';

const id = z.string().min(1).max(256);
const path = z.string().min(1).max(4096);
const empty = z.object({}).strict();
const slot = z.object({ slot: id }).strict();
const define = (name, description, inputSchema, run) => ({ name, description, inputSchema, run: async (input) => result(await run(input)) });

export function tools(api) {
  const definitions = [
    define('husklet_workspace_info', 'Describe the hosting workspace.', empty, () => api.info()),
    define('husklet_workspace_list', 'List bounded workspace summaries.', empty, () => api.list()),
    define('husklet_workspace_inspect', 'Inspect one named workspace.', z.object({ name: id }).strict(), ({ name }) => api.inspect(name)),
    ...['start', 'stop', 'restart'].map((action) => define(`husklet_workspace_${action}`, `${action} a named workspace.`, z.object({ name: id }).strict(), async ({ name }) => { await api[action](name); return { done: true }; })),
    define('husklet_workspace_delete', 'Delete a stopped workspace after explicit confirmation.', z.object({ name: id, confirm: z.literal(true) }).strict(), async ({ name }) => { await api.delete(name); return { done: true }; }),
    define('husklet_container_list', 'List containers.', empty, () => api.containers.list()),
    define('husklet_container_inspect', 'Inspect one container.', z.object({ id }).strict(), ({ id: value }) => api.containers.inspect(value)),
    define('husklet_container_processes', 'Read the bounded process table for one container.', z.object({ id }).strict(), ({ id: value }) => api.containers.processes(value)),
    define('husklet_container_logs', 'Read bounded container logs.', z.object({ id, stdout: z.boolean().default(true), stderr: z.boolean().default(true) }).strict(), ({ id: value, stdout, stderr }) => api.containers.logs(value, { stdout, stderr })),
    ...['start', 'stop', 'pause', 'unpause', 'restart'].map((action) => define(`husklet_container_${action}`, `${action} one container.`, z.object({ id }).strict(), async ({ id: value }) => { await api.containers[action](value); return { done: true }; })),
    define('husklet_container_remove', 'Remove one container after explicit confirmation.', z.object({ id, confirm: z.literal(true) }).strict(), async ({ id: value }) => { await api.containers.remove(value); return { done: true }; }),
    define('husklet_container_kill', 'Signal one container; signal must be explicit.', z.object({ id, signal: z.string().min(1).max(32) }).strict(), async ({ id: value, signal }) => { await api.containers.kill(value, signal); return { done: true }; }),
    define('husklet_terminal_tabs', 'List terminal tabs.', empty, () => api.terminal.tabs()),
    define('husklet_terminal_topology', 'Read terminal split topology.', empty, () => api.terminal.topology()),
    define('husklet_terminal_read', 'Read at most 500 lines from one pane.', z.object({ slot: id, lines: z.number().int().min(1).max(500) }).strict(), ({ slot: value, lines }) => api.terminal.read(value, lines)),
    define('husklet_terminal_write', 'Write bounded literal input to a pane; this does not spawn a shell command.', z.object({ slot: id, input: z.string().max(8192) }).strict(), async ({ slot: value, input }) => { await api.terminal.writeInput(value, input); return { done: true }; }),
    define('husklet_terminal_open', 'Open a terminal tab.', z.object({ title: z.string().max(256).optional() }).strict(), ({ title }) => api.terminal.openTab(title ?? null)),
    define('husklet_terminal_split', 'Split a pane in an explicit direction.', z.object({ slot: id, division: z.enum(['horizontal', 'vertical']) }).strict(), ({ slot: value, division }) => api.terminal.split(value, division)),
    define('husklet_terminal_focus', 'Focus one pane.', slot, async ({ slot: value }) => { await api.terminal.focus(value); return { done: true }; }),
    define('husklet_file_list', 'List a workspace-relative directory.', z.object({ path }).strict(), ({ path: value }) => api.files.list(value)),
    define('husklet_file_read', 'Read one bounded workspace-relative file.', z.object({ path }).strict(), ({ path: value }) => api.files.read(value)),
    define('husklet_file_write', 'Write bounded UTF-8 contents to a workspace-relative file.', z.object({ path, contents: z.string().max(64 * 1024) }).strict(), async ({ path: value, contents }) => { await api.files.write(value, new TextEncoder().encode(contents)); return { done: true }; }),
  ];
  if (typeof api.watchPaneChanges === 'function') definitions.push(define(
    'husklet_pane_wait',
    'Wait for bounded pane-change metadata; fetch a snapshot after notification.',
    z.object({ slot: id.optional(), timeout_ms: z.number().int().min(1).max(30_000).default(30_000) }).strict(),
    ({ slot: wanted, timeout_ms: timeout }) => new Promise((resolve, reject) => {
      let stop;
      let settled = false;
      const finish = (value, error) => {
        if (settled) return;
        settled = true;
        clearTimeout(timer);
        Promise.resolve(stop?.()).then(() => error ? reject(error) : resolve(value), reject);
      };
      const timer = setTimeout(() => finish({ changed: false }), timeout);
      api.watchPaneChanges((change) => {
        if (wanted == null || change.slot === wanted) finish({ changed: true, change });
      }).then((dispose) => { stop = dispose; if (settled) void dispose(); }, (error) => finish(undefined, error));
    }),
  ));
  return definitions.concat(paneTools(api.terminal));
}

export function createServer(session) {
  const server = new McpServer({ name: '@husklet/mcp', version: '0.1.0' });
  for (const tool of tools(workspace(session))) {
    server.registerTool(tool.name, { description: tool.description, inputSchema: tool.inputSchema }, tool.run);
  }
  return server;
}

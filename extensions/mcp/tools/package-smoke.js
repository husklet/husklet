import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const scratch = fs.mkdtempSync(path.join(os.tmpdir(), 'husklet-mcp-pack-'));
try {
  const packed = JSON.parse(execFileSync('npm', ['pack', '--json', '--ignore-scripts', '--pack-destination', scratch], { cwd: root, encoding: 'utf8' }));
  const tarball = path.join(scratch, packed[0].filename);
  const reactRoot = path.resolve(root, '../react');
  const reactPacked = JSON.parse(execFileSync('npm', ['pack', '--json', '--ignore-scripts', '--pack-destination', scratch], { cwd: reactRoot, encoding: 'utf8' }));
  const reactTarball = path.join(scratch, reactPacked[0].filename);
  const consumer = path.join(scratch, 'consumer');
  fs.mkdirSync(consumer);
  fs.writeFileSync(path.join(consumer, 'package.json'), JSON.stringify({ private: true, type: 'module' }));
  execFileSync('npm', ['install', '--ignore-scripts', '--no-audit', '--no-fund', reactTarball, tarball], { cwd: consumer, stdio: 'pipe' });
  const help = execFileSync(path.join(consumer, 'node_modules', '.bin', 'husklet-mcp'), ['--help'], {
    cwd: consumer, encoding: 'utf8',
  });
  assert.match(help, /^Usage: husklet-mcp --socket PATH --workspace NAME/m);
  execFileSync(process.execPath, ['--input-type=module', '--eval', `
    import { tools, createServer, semanticXml } from '@husklet/mcp';
    import { runPaneAgentTurn } from '@husklet/mcp/examples/agent-pane-flow.mjs';
    if (typeof tools !== 'function' || typeof createServer !== 'function') process.exit(1);
    if (typeof runPaneAgentTurn !== 'function') process.exit(1);
    const names = new Set(tools({}).map(({ name }) => name));
    for (const name of ['husklet_workspace_create', 'husklet_workspace_update', 'husklet_container_execution', 'husklet_execution_signal', 'husklet_image_list', 'husklet_image_inspect', 'husklet_image_pull', 'husklet_image_remove', 'husklet_image_prune']) {
      if (!names.has(name)) process.exit(1);
    }
    for (const name of ['husklet_volume_list', 'husklet_volume_inspect', 'husklet_volume_create', 'husklet_volume_remove', 'husklet_network_list', 'husklet_network_inspect', 'husklet_network_create', 'husklet_network_remove', 'husklet_network_connect', 'husklet_network_disconnect']) {
      if (!names.has(name)) process.exit(1);
    }
    for (const name of ['husklet_file_mkdir', 'husklet_file_rename', 'husklet_file_remove']) if (!names.has(name)) process.exit(1);
    if (!names.has('husklet_terminal_write_bytes')) process.exit(1);
    let written;
    const byteTool = tools({ terminal: { writeInput: async (slot, input) => { written = [slot, [...input]]; } } })
      .find(({ name }) => name === 'husklet_terminal_write_bytes');
    await byteTool.run({ slot: 'packed', input_base64: 'AAP//g==' });
    if (JSON.stringify(written) !== JSON.stringify(['packed', [0, 3, 255, 254]])) process.exit(1);
    const terminationCalls = [];
    const termination = tools({ containers: {
      stop: async (id) => terminationCalls.push(['stop', id]),
      kill: async (id, signal) => terminationCalls.push(['kill', id, signal]),
    } });
    const stop = termination.find(({ name }) => name === 'husklet_container_stop');
    const kill = termination.find(({ name }) => name === 'husklet_container_kill');
    if (stop.inputSchema.safeParse({ id: 'packed' }).success) process.exit(1);
    if (kill.inputSchema.safeParse({ id: 'packed', signal: 'SIGKILL' }).success) process.exit(1);
    await stop.run({ id: 'packed', confirm: true });
    await kill.run({ id: 'packed', signal: 'SIGKILL', confirm: true });
    if (JSON.stringify(terminationCalls) !== JSON.stringify([['stop', 'packed'], ['kill', 'packed', 'SIGKILL']])) process.exit(1);
    if (!tools({ terminal: { panes: async () => ({ panes: [], truncated: false }) } }).some(({ name }) => name === 'husklet_pane_list')) process.exit(1);
    const xml = semanticXml({ slot: 'packed', revision: 1, truncated: false, root: { id: 0, role: 'column', label: null, value: null, disabled: false, destructive: false, actions: [], children: [] } });
    if (!xml.startsWith('<pane slot="packed"')) process.exit(1);
  `], { cwd: consumer, stdio: 'pipe' });
  fs.writeFileSync(path.join(consumer, 'consumer.ts'), `
    import { createServer, semanticXml, tools, type TerminalWriteBytesInput, type ToolDefinition } from '@husklet/mcp';
    import type { Session } from '@husklet/react';
    declare const session: Session;
    const server = createServer(session);
    const definitions: ToolDefinition[] = tools({} as Parameters<typeof tools>[0]);
    const bytes: TerminalWriteBytesInput = { slot: 'packed', input_base64: 'AAP//g==' };
    const xml: string = semanticXml({ slot: 'typed', revision: 1, truncated: false, root: { id: 0, role: 'column', label: null, value: null, disabled: false, destructive: false, actions: [], children: [] } });
    void server; void definitions; void bytes; void xml;
    // @ts-expect-error a Session is required
    createServer({});
  `);
  execFileSync(path.resolve(root, '../node_modules/.bin/tsc'), [
    '--noEmit', '--strict', '--skipLibCheck', '--target', 'ES2022',
    '--module', 'NodeNext', '--moduleResolution', 'NodeNext', 'consumer.ts',
  ], { cwd: consumer, stdio: 'pipe' });
  const names = new Set(packed[0].files.map(({ path: name }) => name));
  assert(names.has('src/cli.js'));
  assert(names.has('examples/agent-pane-flow.mjs'));
  assert(![...names].some((name) => name.startsWith('test/') || name.startsWith('tools/')));
} finally {
  fs.rmSync(scratch, { recursive: true, force: true });
}

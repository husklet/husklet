import assert from 'node:assert/strict';
import fs from 'node:fs';
import net from 'node:net';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { Client } from '@modelcontextprotocol/sdk/client/index.js';
import { InMemoryTransport } from '@modelcontextprotocol/sdk/inMemory.js';
import { StdioClientTransport } from '@modelcontextprotocol/sdk/client/stdio.js';
import { CONTROL, KIND, Reader, encode } from '../../react/src/wire.js';
import { createServer } from '../src/index.js';
import { runPaneAgentTurn } from '../examples/agent-pane-flow.mjs';

test('agent flow binds reads to actions and waits once for each changed pane', async () => {
  const calls = [];
  const listeners = new Set();
  let terminalWritten = false;
  const pane = { kind: 'pane', focused: true, grid: { columns: 80, rows: 24 }, pane: {
    slot: 'term-1', occupant: 'terminal', working_directory: '/work', command: 'sh', provider: null,
  } };
  const session = {
    onEvent: (listener) => { listeners.add(listener); return () => listeners.delete(listener); },
    call: async (name, argument) => {
      calls.push([name, argument]);
      if (name === 'pane_list') return { reply: 'panes', with: { panes: [
        { slot: 'term-decoy', generation: 8, revision: 8, kind: 'terminal', provider: null, tab: 'other', title: 'Other', focused: false },
        { slot: 'native-decoy', generation: 8, revision: 8, kind: 'native', provider: null, tab: null, title: 'Other UI', focused: false },
        { slot: 'term-1', generation: 1, revision: terminalWritten ? 5 : calls.filter(([called]) => called === 'pane_list').length === 1 ? 3 : 4, kind: 'terminal', provider: null, tab: 't', title: 'Shell', focused: true },
        { slot: 'native-1', generation: 2, revision: 41, kind: 'native', provider: null, tab: null, title: 'Workspace', focused: false },
      ], truncated: false } };
      if (name === 'terminal_topology') return { reply: 'topology', with: {
        active_tab: 't', tabs: [{ id: 't', title: 'Shell', root: pane }],
      } };
      if (name === 'terminal_read_pane') return { reply: 'text', with: { slot: 'term-1', generation: 1, revision: terminalWritten ? 5 : 4, lines: ['ready'], truncated: false } };
      if (name === 'pane_semantic_read') return { reply: 'semantics', with: {
        slot: 'native-1', generation: 2, revision: 41, truncated: false,
        root: { id: 0, role: 'column', label: null, value: null, disabled: false, destructive: false, actions: [], children: [
          { id: 7, role: 'button', label: 'Refresh', value: null, disabled: false, destructive: false, actions: ['invoke'], children: [] },
        ] },
      } };
      if (name === 'terminal_write_pane') {
        terminalWritten = true;
        queueMicrotask(() => { for (const listener of listeners) listener({ snapshot: 'pane_changes', of: {
          slot: 'term-1', kind: 'terminal', revision: 5, generation: 1, coalesced: 1,
        } }); });
        return { reply: 'done' };
      }
      if (name === 'pane_semantic_action') {
        queueMicrotask(() => { for (const listener of listeners) listener({ snapshot: 'pane_changes', of: {
          slot: 'native-1', kind: 'native', revision: 42, generation: 2, coalesced: 3,
        } }); });
        return { reply: 'done' };
      }
      if (name === 'event_subscribe') {
        calls.push(['subscription-ready']);
        queueMicrotask(() => { for (const listener of listeners) listener({ snapshot: 'pane_changes', of: {
          slot: argument.slot, kind: argument.slot === 'term-1' ? 'terminal' : 'native',
          revision: argument.after_revision, generation: argument.after_generation, coalesced: 0,
        } }); });
        return { reply: 'done' };
      }
      if (name === 'event_unsubscribe') return { reply: 'done' };
      throw new Error(`unexpected call ${name}`);
    },
  };
  const server = createServer(session);
  const client = new Client({ name: 'agent-flow-test', version: '1' });
  const [clientTransport, serverTransport] = InMemoryTransport.createLinkedPair();
  await Promise.all([server.connect(serverTransport), client.connect(clientTransport)]);

  const answer = await runPaneAgentTurn(client, { terminalSlot: 'term-1', semanticSlot: 'native-1', terminalBytes: Uint8Array.from([0, 3, 255]), waitMs: 1000 });
  assert.equal(answer.semantic.revision, 41);
  assert.equal(answer.semantic.generation, 2);
  assert.equal(answer.semantic.node, 7);
  assert.equal(answer.terminal.slot, 'term-1');
  assert.equal(answer.semantic.slot, 'native-1');
  assert.equal(answer.semantic.changed.change.coalesced, 3);
  assert.equal(answer.semantic.changed.change.revision, 42, 'the unchanged initial cursor did not settle the wait');
  assert.equal(answer.terminal.revision, 4, 'write uses the cursor returned by the terminal read');
  assert.equal(answer.terminal.changed.change.revision, 5);
  assert.match(answer.terminal.after, /revision="5"/);
  assert.match(answer.semantic.after, /revision="41"/);
  assert(calls.some(([name, argument]) => name === 'terminal_write_pane'
    && argument.slot === 'term-1' && argument.revision === 4 && Buffer.from(argument.contents).equals(Buffer.from([0, 3, 255]))));
  assert(calls.some(([name, argument]) => name === 'pane_semantic_action'
    && argument.action.generation === 2 && argument.action.revision === 41 && argument.action.node === 7));
  assert.equal(calls.filter(([name]) => name === 'pane_semantic_read').length, 3);
  assert.equal(calls.filter(([name]) => name === 'event_subscribe').length, 2);
  assert.equal(calls.filter(([name]) => name === 'event_unsubscribe').length, 2);
  assert(calls.some(([name, argument]) => name === 'event_subscribe'
    && argument.topic === 'pane-changes'));

  await client.close();
  await server.close();
});

test('packaged CLI drives exact selected panes over a real Unix socket', async (context) => {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'husklet-pane-flow-cli-')); const socketPath = path.join(directory, 'host.sock');
  let terminalRevision = 4; let semanticRevision = 41;
  const host = net.createServer((socket) => { const reader = new Reader(); socket.write(encode({ channel: CONTROL, kind: KIND.request, payload: { protocol: 1, extension: 'agent', granted: ['workspace-read', 'terminal-read', 'terminal-control', 'pane-observe', 'pane-semantic-read', 'pane-semantic-control'] } })); socket.on('data', (chunk) => { for (const frame of reader.take(chunk)) {
    if (frame.kind !== KIND.request || frame.channel === CONTROL) continue; const call = frame.payload.call; let payload;
    if (call === 'workspace_info') payload = { reply: 'workspace', with: { name: 'dev' } };
    else if (call === 'pane_list') payload = { reply: 'panes', with: { panes: [{ slot: 'decoy', generation: 9, revision: 9, kind: 'terminal' }, { slot: 'chosen-term', generation: 1, revision: terminalRevision, kind: 'terminal' }, { slot: 'chosen-ui', generation: 2, revision: semanticRevision, kind: 'native' }], truncated: false } };
    else if (call === 'terminal_topology') payload = { reply: 'topology', with: { active_tab: 'build', tabs: [{ id: 'build', title: 'Build', root: { kind: 'pane', pane: { slot: 'chosen-term', occupant: 'terminal', working_directory: '/work', command: 'sh', provider: null }, grid: { columns: 100, rows: 30 }, focused: true } }] } };
    else if (call === 'terminal_read_pane') payload = { reply: 'text', with: { slot: 'chosen-term', generation: 1, revision: terminalRevision, lines: ['interpreted screen'], truncated: false, cursor_column: 3, cursor_row: 2, columns: 100, rows: 30 } };
    else if (call === 'pane_semantic_read') payload = { reply: 'semantics', with: { slot: 'chosen-ui', generation: 2, revision: semanticRevision, truncated: false, root: { id: 0, role: 'button', label: 'Refresh', value: null, disabled: false, destructive: false, actions: ['invoke'], children: [] } } };
    else if (call === 'terminal_write_pane') { terminalRevision += 1; payload = { reply: 'done' }; }
    else if (call === 'pane_semantic_action') { semanticRevision += 1; payload = { reply: 'done' }; }
    else if (call === 'event_subscribe' || call === 'event_unsubscribe') payload = { reply: 'done' };
    else throw new Error(`unexpected ${call}`);
    socket.write(encode({ channel: frame.channel, kind: KIND.response, payload }));
    if (call === 'event_subscribe') setImmediate(() => socket.write(encode({ channel: 21, kind: KIND.event, payload: { snapshot: 'pane_changes', of: { slot: frame.payload.with.slot, kind: frame.payload.with.slot === 'chosen-term' ? 'terminal' : 'native', generation: frame.payload.with.after_generation, revision: frame.payload.with.after_revision, coalesced: 0 } } })));
    if (call === 'terminal_write_pane' || call === 'pane_semantic_action') setImmediate(() => socket.write(encode({ channel: 21, kind: KIND.event, payload: { snapshot: 'pane_changes', of: { slot: call === 'terminal_write_pane' ? 'chosen-term' : 'chosen-ui', kind: call === 'terminal_write_pane' ? 'terminal' : 'native', generation: call === 'terminal_write_pane' ? 1 : 2, revision: call === 'terminal_write_pane' ? terminalRevision : semanticRevision, coalesced: 1 } } })));
  } }); });
  await new Promise((resolve, reject) => host.listen(socketPath, resolve).once('error', reject));
  const transport = new StdioClientTransport({ command: process.execPath, args: [path.resolve(import.meta.dirname, '../src/cli.js'), '--socket', socketPath, '--workspace', 'dev'], cwd: path.resolve(import.meta.dirname, '..'), stderr: 'pipe' });
  const client = new Client({ name: 'packaged-pane-flow', version: '1' });
  context.after(async () => { await client.close(); await new Promise((resolve) => host.close(resolve)); fs.rmSync(directory, { recursive: true, force: true }); });
  await client.connect(transport);
  const answer = await runPaneAgentTurn(client, { terminalSlot: 'chosen-term', semanticSlot: 'chosen-ui', terminalBytes: Uint8Array.of(3), waitMs: 1000 });
  assert.equal(answer.terminal.slot, 'chosen-term'); assert.match(answer.terminal.before, /active="true"/); assert.match(answer.terminal.before, /focused="true"/); assert.match(answer.terminal.before, /cursor-column="3"/); assert.match(answer.semantic.before, /<label>Refresh<\/label>/);
});

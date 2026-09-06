import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import net from 'node:net';
import os from 'node:os';
import path from 'node:path';
import { mkdtemp, rm } from 'node:fs/promises';
import test from 'node:test';
import { fileURLToPath } from 'node:url';
import { CONTROL, KIND, Reader, encode } from '../src/wire.js';

const example = fileURLToPath(new URL('../examples/agent-control.mjs', import.meta.url));

test('packaged external agent controls terminal bytes and semantic UI over real Unix framing', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-agent-scenario-'));
  const socketPath = path.join(directory, 'host.sock'); const calls = []; const connections = new Set();
  let terminalReads = 0; let semanticReads = 0;
  const panes = [
    { slot: 'term-1', generation: 3, revision: 7, kind: 'terminal', provider: null, tab: 'tab-1', title: 'Shell', focused: true },
    { slot: 'ui-1', generation: 2, revision: 4, kind: 'native', provider: null, tab: null, title: 'Settings', focused: false },
  ];
  const semantic = (revision, value) => ({ slot: 'ui-1', generation: 2, revision, truncated: false, root: {
    id: 0, role: 'page', label: 'Settings', value: null, disabled: false, destructive: false, actions: [], children: [{
      id: 7, role: 'button', label: 'Toggle', value, disabled: false, destructive: false, actions: ['invoke'], children: [],
    }],
  } });
  const server = net.createServer((socket) => {
    connections.add(socket); socket.on('close', () => connections.delete(socket)); const reader = new Reader();
    socket.on('data', (chunk) => { for (const frame of reader.take(chunk)) {
      if (frame.channel !== 2) continue; const call = frame.payload.call; calls.push(call);
      if (call === 'event_subscribe' || call === 'event_unsubscribe') {
        socket.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
      } else if (call === 'pane_list') {
        socket.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'panes', with: { panes, truncated: false } } }));
      } else if (call === 'terminal_read_pane') {
        terminalReads += 1; const revision = terminalReads < 3 ? 7 : 8;
        socket.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'text', with: {
          slot: 'term-1', generation: 3, revision, columns: 80, rows: 24,
          lines: revision === 7 ? ['$ '] : ['$ ^C'], cursor_column: 0, cursor_row: 1, truncated: false,
        } } }));
      } else if (call === 'terminal_write_pane') {
        assert.deepEqual(frame.payload.with, { slot: 'term-1', generation: 3, revision: 7, contents: [0, 3, 255] });
        socket.write(encode({ channel: 201, kind: KIND.event, payload: { snapshot: 'pane_changes', of: {
          slot: 'term-1', kind: 'terminal', generation: 3, revision: 8, coalesced: 0,
        } } }));
        socket.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
      } else if (call === 'pane_semantic_read') {
        semanticReads += 1; const revision = semanticReads < 3 ? 4 : 5;
        socket.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'semantics', with: semantic(revision, revision === 4 ? 'off' : 'on') } }));
      } else if (call === 'pane_semantic_action') {
        assert.deepEqual(frame.payload.with, { slot: 'ui-1', action: { generation: 2, revision: 4, node: 7, action: 'invoke', value: null } });
        socket.write(encode({ channel: 202, kind: KIND.event, payload: { snapshot: 'pane_changes', of: {
          slot: 'ui-1', kind: 'native', generation: 2, revision: 5, coalesced: 0,
        } } }));
        socket.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
      }
    } });
    socket.write(encode({ channel: CONTROL, kind: KIND.open, payload: { protocol: 1, peer: 'external-agent', granted: ['panes:observe', 'terminals:output', 'terminals:control', 'panes:semantic-read', 'panes:semantic-control'] } }));
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const child = spawn(process.execPath, [example, JSON.stringify({ path: socketPath, terminalSlot: 'term-1', uiSlot: 'ui-1', node: 7, input: [0, 3, 255] })], { stdio: ['ignore', 'pipe', 'pipe'] });
    let stdout = ''; let stderr = ''; child.stdout.setEncoding('utf8'); child.stderr.setEncoding('utf8');
    child.stdout.on('data', (chunk) => { stdout += chunk; }); child.stderr.on('data', (chunk) => { stderr += chunk; });
    const code = await new Promise((resolve) => child.on('close', resolve));
    assert.equal(code, 0, stderr); const result = JSON.parse(stdout);
    assert.equal(result.terminal, '$ '); assert.equal(result.terminalAfter, '$ ^C');
    assert.match(result.ui, /<value>off<\/value>/); assert.match(result.uiAfter, /<value>on<\/value>/);
    assert.deepEqual(calls, [
      'pane_list', 'pane_list', 'terminal_read_pane',
      'event_subscribe', 'terminal_read_pane', 'terminal_write_pane', 'terminal_read_pane', 'event_unsubscribe',
      'pane_list', 'pane_semantic_read',
      'event_subscribe', 'pane_semantic_read', 'pane_semantic_action', 'pane_semantic_read', 'event_unsubscribe',
    ]);
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve)); await rm(directory, { recursive: true, force: true });
  }
});

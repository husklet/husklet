import assert from 'node:assert/strict';
import test from 'node:test';
import { Client } from '@modelcontextprotocol/sdk/client/index.js';
import { InMemoryTransport } from '@modelcontextprotocol/sdk/inMemory.js';
import { createServer } from '../src/index.js';
import { runPaneAgentTurn } from '../examples/agent-pane-flow.mjs';

test('agent flow discovers, observes, writes exact bytes, acts at its observed revision, and waits once', async () => {
  const calls = [];
  const listeners = new Set();
  const pane = { kind: 'pane', focused: true, grid: { columns: 80, rows: 24 }, pane: {
    slot: 'term-1', occupant: 'terminal', working_directory: '/work', command: 'sh', provider: null,
  } };
  const session = {
    onEvent: (listener) => { listeners.add(listener); return () => listeners.delete(listener); },
    call: async (name, argument) => {
      calls.push([name, argument]);
      if (name === 'pane_list') return { reply: 'panes', with: { panes: [
        { slot: 'term-1', generation: 1, revision: 3, kind: 'terminal', provider: null, tab: 't', title: 'Shell', focused: true },
        { slot: 'native-1', generation: 2, revision: 41, kind: 'native', provider: null, tab: null, title: 'Workspace', focused: false },
      ], truncated: false } };
      if (name === 'terminal_topology') return { reply: 'topology', with: {
        active_tab: 't', tabs: [{ id: 't', title: 'Shell', root: pane }],
      } };
      if (name === 'terminal_read_pane') return { reply: 'text', with: { slot: 'term-1', lines: ['ready'], truncated: false } };
      if (name === 'pane_semantic_read') return { reply: 'semantics', with: {
        slot: 'native-1', generation: 2, revision: 41, truncated: false,
        root: { id: 0, role: 'column', label: null, value: null, disabled: false, destructive: false, actions: [], children: [
          { id: 7, role: 'button', label: 'Refresh', value: null, disabled: false, destructive: false, actions: ['invoke'], children: [] },
        ] },
      } };
      if (name === 'terminal_write_pane') return { reply: 'done' };
      if (name === 'pane_semantic_action') {
        queueMicrotask(() => { for (const listener of listeners) listener({ snapshot: 'pane_changes', of: {
          slot: 'native-1', kind: 'native', revision: 42, generation: 2, coalesced: 3,
        } }); });
        return { reply: 'done' };
      }
      if (name === 'event_subscribe') {
        calls.push(['subscription-ready']);
        queueMicrotask(() => { for (const listener of listeners) listener({ snapshot: 'pane_changes', of: {
          slot: 'native-1', kind: 'native', revision: 41, generation: 2, coalesced: 0,
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

  const answer = await runPaneAgentTurn(client, { terminalBytes: Uint8Array.from([0, 3, 255]), waitMs: 1000 });
  assert.equal(answer.semantic.revision, 41);
  assert.equal(answer.semantic.generation, 2);
  assert.equal(answer.semantic.node, 7);
  assert.equal(answer.semantic.changed.change.coalesced, 3);
  assert.equal(answer.semantic.changed.change.revision, 42, 'the unchanged initial cursor did not settle the wait');
  assert.match(answer.semantic.after, /revision="41"/);
  assert(calls.some(([name, argument]) => name === 'terminal_write_pane'
    && argument.slot === 'term-1' && Buffer.from(argument.contents).equals(Buffer.from([0, 3, 255]))));
  assert(calls.some(([name, argument]) => name === 'pane_semantic_action'
    && argument.action.generation === 2 && argument.action.revision === 41 && argument.action.node === 7));
  assert.equal(calls.filter(([name]) => name === 'pane_semantic_read').length, 3);
  assert.equal(calls.filter(([name]) => name === 'event_subscribe').length, 1);
  assert.equal(calls.filter(([name]) => name === 'event_unsubscribe').length, 1);
  assert(calls.some(([name, argument]) => name === 'event_subscribe'
    && argument.topic === 'pane-changes'));

  await client.close();
  await server.close();
});

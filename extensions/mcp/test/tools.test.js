import assert from 'node:assert/strict';
import test from 'node:test';
import { Client } from '@modelcontextprotocol/sdk/client/index.js';
import { InMemoryTransport } from '@modelcontextprotocol/sdk/inMemory.js';
import { createServer, semanticXml, tools } from '../src/index.js';

function fake() {
  const calls = [];
  const record = (name, answer = { ok: true }) => async (...args) => { calls.push([name, ...args]); return answer; };
  return { calls, api: {
    info: record('info', { name: 'demo', token: 'never expose me' }), list: record('list'), inspect: record('inspect'),
    start: record('workspace.start'), stop: record('workspace.stop'), restart: record('workspace.restart'), delete: record('workspace.delete'),
    containers: { list: record('containers.list'), inspect: record('containers.inspect'), processes: record('containers.processes'), logs: record('containers.logs'), start: record('containers.start'), stop: record('containers.stop'), pause: record('containers.pause'), unpause: record('containers.unpause'), restart: record('containers.restart'), remove: record('containers.remove'), kill: record('containers.kill') },
    terminal: { tabs: record('terminal.tabs'), topology: record('terminal.topology'), read: record('terminal.read'), writeInput: record('terminal.writeInput'), openTab: record('terminal.openTab'), split: record('terminal.split'), focus: record('terminal.focus') },
    files: { list: record('files.list'), read: record('files.read'), write: record('files.write') },
  }};
}

test('schemas are strict, controls map exactly, and no shell shortcut exists', async () => {
  const { api, calls } = fake();
  const listed = tools(api);
  assert(!listed.some(({ name }) => /exec|spawn|shell/.test(name)));
  const start = listed.find(({ name }) => name === 'husklet_container_start');
  assert.equal(start.inputSchema.safeParse({ id: 'abc', extra: true }).success, false);
  await start.run({ id: 'abc' });
  assert.deepEqual(calls, [['containers.start', 'abc']]);
});

test('results redact secrets and remain bounded', async () => {
  const { api } = fake();
  const info = tools(api).find(({ name }) => name === 'husklet_workspace_info');
  const answer = await info.run({});
  assert.equal(answer.content[0].text, '{"name":"demo","token":"[redacted]"}');
  api.files.read = async () => 'x'.repeat(100_000);
  const read = tools(api).find(({ name }) => name === 'husklet_file_read');
  const bounded = await read.run({ path: 'notes.txt' });
  assert(bounded.content[0].text.length <= 64 * 1024);
  assert.match(bounded.content[0].text, /truncated/);
});

test('pane tools are capability-shaped and only appear for the real typed methods', async () => {
  const { api, calls } = fake();
  assert(!tools(api).some(({ name }) => name.startsWith('husklet_pane_')));
  api.terminal.semantics = async (slot) => { calls.push(['terminal.semantics', slot]); return {
    slot, revision: 7, truncated: false,
    root: { id: 0, role: 'column', label: 'A & <B>', value: null, disabled: false, actions: ['invoke'], children: [] },
  }; };
  api.terminal.act = async (slot, action) => { calls.push(['terminal.act', slot, action]); };
  const listed = tools(api);
  const snapshot = listed.find(({ name }) => name === 'husklet_pane_snapshot');
  const action = listed.find(({ name }) => name === 'husklet_pane_action');
  const shown = await snapshot.run({ slot: 'pane-1' });
  assert.equal(shown.content[0].text, '<pane slot="pane-1" revision="7" truncated="false"><node id="0" role="column" disabled="false" actions="invoke"><label>A &amp; &lt;B&gt;</label></node></pane>');
  await action.run({ slot: 'pane-1', revision: 7, node: 3, action: 'invoke' });
  assert.deepEqual(calls, [
    ['terminal.semantics', 'pane-1'],
    ['terminal.act', 'pane-1', { revision: 7, node: 3, action: 'invoke' }],
  ]);
  assert.equal(action.inputSchema.safeParse({ slot: 'pane-1', revision: 7, node: 3, action: 'run' }).success, false);
});

test('semantic XML escapes every XML metacharacter and remains structurally bounded', () => {
  const hostile = `&<>"'`;
  assert.equal(semanticXml({ slot: hostile, revision: 3, truncated: false, root: {
    id: hostile, role: hostile, label: hostile, value: '[redacted]', disabled: true,
    actions: [hostile], children: [],
  }}), '<pane slot="&amp;&lt;&gt;&quot;&apos;" revision="3" truncated="false"><node id="&amp;&lt;&gt;&quot;&apos;" role="&amp;&lt;&gt;&quot;&apos;" disabled="true" actions="&amp;&lt;&gt;&quot;&apos;"><label>&amp;&lt;&gt;&quot;&apos;</label><value>[redacted]</value></node></pane>');
  const secret = semanticXml({ slot: 's', revision: 1, truncated: false, root: {
    id: 1, role: 'password_entry', label: 'Password', value: 'must-not-leak', disabled: false, actions: [], children: [],
  }});
  assert(!secret.includes('must-not-leak'));
  assert.match(secret, /<value>\[redacted\]<\/value>/);
  const controls = semanticXml({ slot: '\u0000\uD800', revision: 1, truncated: false, root: {
    id: 1, role: 'text', label: '\u0001', value: null, disabled: false, actions: [], children: [],
  }});
  assert(!/[\u0000-\u0008\uD800-\uDFFF]/.test(controls));
  assert.match(controls, /�/);

  const children = Array.from({ length: 400 }, (_, id) => ({
    id, role: 'text', label: 'x'.repeat(1000), value: null, disabled: false, actions: [], children: [],
  }));
  const bounded = semanticXml({ slot: 'large', revision: 4, truncated: true, root: {
    id: 0, role: 'column', label: null, value: null, disabled: false, actions: [], children,
  }});
  assert(new TextEncoder().encode(bounded).byteLength <= 64 * 1024);
  assert.match(bounded, /<truncated\/>/);
  assert.match(bounded, /^<pane .*<\/pane>$/);
  assert.equal((bounded.match(/<node /g) ?? []).length, (bounded.match(/<\/node>/g) ?? []).length);
});

test('a real MCP client lists strict tools and calls through the React session contract', async () => {
  const calls = [];
  const session = {
    call: async (name, argument) => {
      calls.push([name, argument]);
      if (name === 'workspace_info') return { reply: 'workspace', with: { name: 'demo' } };
      if (name === 'pane_semantic_read') return { reply: 'semantics', with: {
        slot: argument.slot, revision: 11, truncated: false,
        root: { id: 0, role: 'column', label: 'Live', value: null, disabled: false, actions: [], children: [] },
      } };
      if (name === 'pane_semantic_action') return { reply: 'done' };
      throw new Error(`unexpected call ${name}`);
    },
  };
  const server = createServer(session);
  const client = new Client({ name: 'test', version: '1' });
  const [clientTransport, serverTransport] = InMemoryTransport.createLinkedPair();
  await Promise.all([server.connect(serverTransport), client.connect(clientTransport)]);
  const listed = await client.listTools();
  assert(listed.tools.some(({ name }) => name === 'husklet_workspace_info'));
  assert(listed.tools.some(({ name }) => name === 'husklet_pane_snapshot'));
  assert(listed.tools.some(({ name }) => name === 'husklet_pane_action'));
  const answer = await client.callTool({ name: 'husklet_workspace_info', arguments: {} });
  assert.equal(answer.content[0].text, '{"name":"demo"}');
  const snapshot = await client.callTool({ name: 'husklet_pane_snapshot', arguments: { slot: 'pane-live' } });
  assert.match(snapshot.content[0].text, /^<pane slot="pane-live" revision="11"/);
  await client.callTool({ name: 'husklet_pane_action', arguments: { slot: 'pane-live', revision: 11, node: 0, action: 'invoke' } });
  assert.deepEqual(calls, [
    ['workspace_info', undefined],
    ['pane_semantic_read', { slot: 'pane-live' }],
    ['pane_semantic_action', { slot: 'pane-live', action: { revision: 11, node: 0, action: 'invoke' } }],
  ]);
  await client.close();
  await server.close();
});

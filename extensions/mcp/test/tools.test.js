import assert from 'node:assert/strict';
import test from 'node:test';
import { Client } from '@modelcontextprotocol/sdk/client/index.js';
import { InMemoryTransport } from '@modelcontextprotocol/sdk/inMemory.js';
import { createServer, paneXml, semanticXml, tools } from '../src/index.js';

function fake() {
  const calls = [];
  const record = (name, answer = { ok: true }) => async (...args) => { calls.push([name, ...args]); return answer; };
  return { calls, api: {
    info: record('info', { name: 'demo', token: 'never expose me' }), list: record('list'), inspect: record('inspect'),
    start: record('workspace.start'), stop: record('workspace.stop'), restart: record('workspace.restart'), delete: record('workspace.delete'),
    containers: { list: record('containers.list'), inspect: record('containers.inspect'), processes: record('containers.processes'), execution: record('containers.execution'), logs: record('containers.logs'), create: record('containers.create'), exec: record('containers.exec'), start: record('containers.start'), stop: record('containers.stop'), pause: record('containers.pause'), unpause: record('containers.unpause'), restart: record('containers.restart'), remove: record('containers.remove'), kill: record('containers.kill') },
    images: { list: record('images.list'), inspect: record('images.inspect'), pull: record('images.pull'), remove: record('images.remove'), prune: record('images.prune') },
    volumes: { list: record('volumes.list'), inspect: record('volumes.inspect'), create: record('volumes.create'), remove: record('volumes.remove') },
    networks: { list: record('networks.list'), inspect: record('networks.inspect'), create: record('networks.create'), remove: record('networks.remove'), connect: record('networks.connect'), disconnect: record('networks.disconnect') },
    terminal: { tabs: record('terminal.tabs'), topology: record('terminal.topology'), read: record('terminal.read'), writeInput: record('terminal.writeInput'), openTab: record('terminal.openTab'), split: record('terminal.split'), focus: record('terminal.focus'), resizeGrid: record('terminal.resizeGrid'), ratio: record('terminal.ratio'), close: record('terminal.close') },
    files: { list: record('files.list'), read: record('files.read'), write: record('files.write') },
  }};
}

test('schemas are strict, controls map exactly, and no terminal shell shortcut exists', async () => {
  const { api, calls } = fake();
  const listed = tools(api);
  assert(!listed.some(({ name }) => /spawn|shell/.test(name)));
  const start = listed.find(({ name }) => name === 'husklet_container_start');
  assert.equal(start.inputSchema.safeParse({ id: 'abc', extra: true }).success, false);
  await start.run({ id: 'abc' });
  assert.deepEqual(calls, [['containers.start', 'abc']]);
});

test('container create and exec accept only bounded structured authority', async () => {
  const { api, calls } = fake();
  const listed = tools(api);
  const create = listed.find(({ name }) => name === 'husklet_container_create');
  const exec = listed.find(({ name }) => name === 'husklet_container_exec');
  assert.equal(create.inputSchema.safeParse({ image: 'alpine:3.20', name: 'worker-1' }).success, true);
  assert.equal(create.inputSchema.safeParse({ image: 'alpine latest', name: 'worker' }).success, false);
  assert.equal(create.inputSchema.safeParse({ image: 'alpine:3.20', name: '../worker' }).success, false);
  assert.equal(exec.inputSchema.safeParse({ id: 'c1', command: 'sh -lc whoami' }).success, false);
  assert.equal(exec.inputSchema.safeParse({ id: 'c1', command: [] }).success, false);
  assert.equal(exec.inputSchema.safeParse({ id: 'c1', command: Array(65).fill('x') }).success, false);
  assert.equal(exec.inputSchema.safeParse({ id: 'c1', command: ['true'], working_directory: 'relative' }).success, false);
  await create.run({ image: 'alpine:3.20', name: 'worker-1' });
  await exec.run({ id: 'c1', command: ['printf', '%s', 'hello'], user: '1000', working_directory: '/work' });
  assert.deepEqual(calls, [
    ['containers.create', 'alpine:3.20', 'worker-1'],
    ['containers.exec', 'c1', { command: ['printf', '%s', 'hello'], user: '1000', workingDirectory: '/work' }],
  ]);
});

test('container execution inspection is a strict bounded read through the typed API', async () => {
  const { api, calls } = fake();
  const execution = tools(api).find(({ name }) => name === 'husklet_container_execution');
  assert(execution);
  assert.equal(execution.inputSchema.safeParse({ id: 'exec-1', extra: true }).success, false);
  await execution.run({ id: 'exec-1' });
  assert.deepEqual(calls, [['containers.execution', 'exec-1']]);
});

test('terminal layout tools use the host wire vocabulary and bounded destructive controls', async () => {
  const { api, calls } = fake();
  const listed = tools(api);
  const split = listed.find(({ name }) => name === 'husklet_terminal_split');
  const resize = listed.find(({ name }) => name === 'husklet_terminal_resize');
  const ratio = listed.find(({ name }) => name === 'husklet_terminal_ratio');
  const close = listed.find(({ name }) => name === 'husklet_terminal_close');
  assert.equal(split.inputSchema.safeParse({ slot: 'pane-1', division: 'horizontal' }).success, false);
  assert.equal(split.inputSchema.safeParse({ slot: 'pane-1', division: 'beside' }).success, true);
  assert.equal(resize.inputSchema.safeParse({ slot: 'pane-1', columns: 0, rows: 24 }).success, false);
  assert.equal(ratio.inputSchema.safeParse({ slot: 'pane-1', ratio: 0.99 }).success, false);
  assert.equal(close.inputSchema.safeParse({ slot: 'pane-1' }).success, false);
  await split.run({ slot: 'pane-1', division: 'below' });
  await resize.run({ slot: 'pane-1', columns: 120, rows: 40 });
  await ratio.run({ slot: 'pane-1', ratio: 0.6 });
  await close.run({ slot: 'pane-1', confirm: true });
  assert.deepEqual(calls, [
    ['terminal.split', 'pane-1', 'below'],
    ['terminal.resizeGrid', 'pane-1', 120, 40],
    ['terminal.ratio', 'pane-1', 0.6],
    ['terminal.close', 'pane-1'],
  ]);
});

test('image tools use typed reads and require confirmation for destructive controls', async () => {
  const { api, calls } = fake();
  const listed = tools(api);
  const byName = (name) => listed.find((tool) => tool.name === name);
  assert.equal(byName('husklet_image_inspect').inputSchema.safeParse({ reference: 'a'.repeat(257) }).success, false);
  assert.equal(byName('husklet_image_remove').inputSchema.safeParse({ reference: 'alpine:3.20' }).success, false);
  assert.equal(byName('husklet_image_prune').inputSchema.safeParse({ confirm: false }).success, false);
  await byName('husklet_image_list').run({});
  await byName('husklet_image_inspect').run({ reference: 'sha256:abc' });
  await byName('husklet_image_pull').run({ reference: 'alpine:3.20' });
  await byName('husklet_image_remove').run({ reference: 'old:tag', confirm: true });
  await byName('husklet_image_prune').run({ confirm: true });
  assert.deepEqual(calls, [
    ['images.list'],
    ['images.inspect', 'sha256:abc'],
    ['images.pull', 'alpine:3.20'],
    ['images.remove', 'old:tag'],
    ['images.prune'],
  ]);
});

test('volume and network tools preserve typed read/control operations and confirmations', async () => {
  const { api, calls } = fake();
  const listed = tools(api);
  const byName = (name) => listed.find((tool) => tool.name === name);
  assert.equal(byName('husklet_volume_remove').inputSchema.safeParse({ name: 'cache' }).success, false);
  assert.equal(byName('husklet_network_remove').inputSchema.safeParse({ reference: 'private' }).success, false);
  assert.equal(byName('husklet_network_disconnect').inputSchema.safeParse({ reference: 'private', container: 'c1' }).success, false);
  assert.equal(byName('husklet_network_connect').inputSchema.safeParse({ reference: 'private', container: 'c1', extra: true }).success, false);
  await byName('husklet_volume_list').run({});
  await byName('husklet_volume_inspect').run({ name: 'cache' });
  await byName('husklet_volume_create').run({ name: 'build' });
  await byName('husklet_volume_remove').run({ name: 'old', confirm: true });
  await byName('husklet_network_list').run({});
  await byName('husklet_network_inspect').run({ reference: 'private' });
  await byName('husklet_network_create').run({ name: 'backend' });
  await byName('husklet_network_remove').run({ reference: 'old-net', confirm: true });
  await byName('husklet_network_connect').run({ reference: 'backend', container: 'c1' });
  await byName('husklet_network_disconnect').run({ reference: 'backend', container: 'c1', confirm: true });
  assert.deepEqual(calls, [
    ['volumes.list'], ['volumes.inspect', 'cache'], ['volumes.create', 'build'], ['volumes.remove', 'old'],
    ['networks.list'], ['networks.inspect', 'private'], ['networks.create', 'backend'], ['networks.remove', 'old-net'],
    ['networks.connect', 'backend', 'c1'], ['networks.disconnect', 'backend', 'c1'],
  ]);
});

test('unified pane XML packs terminal metadata and escaped bounded screen lines', async () => {
  const terminal = {
    topology: async () => ({ active_tab: 'tab-1', tabs: [{ id: 'tab-1', title: 'Shell & work', root: {
      kind: 'pane', focused: true, grid: { columns: 120, rows: 40 },
      pane: { slot: 'term-1', occupant: 'terminal', working_directory: '/work<&>', command: 'bash', provider: null },
    } }] }),
    read: async () => ({ slot: 'term-1', lines: ['one < two', 'token output remains screen data'], truncated: false }),
    semantics: async () => { throw new Error('not semantic'); },
  };
  const xml = await paneXml(terminal, 'term-1', 20);
  assert.match(xml, /^<husklet-pane slot="term-1" occupant="terminal"><terminal /);
  assert.match(xml, /active="true" focused="true" columns="120" rows="40"/);
  assert.match(xml, /title="Shell &amp; work"/);
  assert.match(xml, /<line index="0">one &lt; two<\/line>/);
  assert.match(xml, /token output remains screen data/);
  assert(new TextEncoder().encode(xml).byteLength <= 64 * 1024);
  assert.match(xml, /<\/terminal><\/husklet-pane>$/);
});

test('unified pane XML selects surface semantics and gives a clear absent error', async () => {
  const terminal = {
    topology: async () => ({ active_tab: null, tabs: [{ id: 't', title: 'UI', root: {
      kind: 'pane', focused: false, grid: null,
      pane: { slot: 'surface-1', occupant: 'surface', working_directory: null, command: null, provider: { extension: 'demo', provider: 'main' } },
    } }] }),
    semantics: async (slot) => {
      if (slot === 'missing') throw new Error('no semantic pane');
      return { slot, revision: 2, truncated: false, root: {
        id: 1, role: 'password_entry', label: 'API token', value: 'never leak', disabled: false, actions: [], children: [],
      } };
    },
  };
  const xml = await paneXml(terminal, 'surface-1');
  assert.match(xml, /^<husklet-pane slot="surface-1" occupant="surface"><pane /);
  assert(!xml.includes('never leak'));
  assert.match(xml, /\[redacted\]/);
  await assert.rejects(() => paneXml(terminal, 'missing'), /absent from topology and exposes no native semantics/);
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
    root: { id: 0, role: 'column', label: 'A & <B>', value: null, disabled: false, destructive: false, actions: ['invoke'], children: [] },
  }; };
  api.terminal.act = async (slot, action) => { calls.push(['terminal.act', slot, action]); };
  const listed = tools(api);
  const snapshot = listed.find(({ name }) => name === 'husklet_pane_snapshot');
  const action = listed.find(({ name }) => name === 'husklet_pane_action');
  const shown = await snapshot.run({ slot: 'pane-1' });
  assert.equal(shown.content[0].text, '<pane slot="pane-1" revision="7" truncated="false"><node id="0" role="column" disabled="false" destructive="false" actions="invoke"><label>A &amp; &lt;B&gt;</label></node></pane>');
  await action.run({ slot: 'pane-1', revision: 7, node: 3, action: 'invoke' });
  assert.deepEqual(calls, [
    ['terminal.semantics', 'pane-1'],
    ['terminal.semantics', 'pane-1'],
    ['terminal.act', 'pane-1', { revision: 7, node: 3, action: 'invoke' }],
  ]);
  assert.equal(action.inputSchema.safeParse({ slot: 'pane-1', revision: 7, node: 3, action: 'run' }).success, false);
});

test('destructive semantic actions require an explicit MCP confirmation', async () => {
  const { api, calls } = fake();
  api.terminal.semantics = async () => ({ slot: 'workspace', revision: 9, truncated: false, root: {
    id: 0, role: 'navigation', label: null, value: null, disabled: false, destructive: false, actions: [], children: [
      { id: 4, role: 'button', label: 'Confirm removal', value: null, disabled: false, destructive: true, actions: ['invoke'], children: [] },
    ],
  }});
  api.terminal.act = async (...args) => calls.push(['terminal.act', ...args]);
  const action = tools(api).find(({ name }) => name === 'husklet_pane_action');
  await assert.rejects(
    action.run({ slot: 'workspace', revision: 9, node: 4, action: 'invoke' }),
    /requires confirm: true/,
  );
  assert(!calls.some(([name]) => name === 'terminal.act'));
  await action.run({ slot: 'workspace', revision: 9, node: 4, action: 'invoke', confirm: true });
  assert.deepEqual(calls.at(-1), ['terminal.act', 'workspace', { revision: 9, node: 4, action: 'invoke' }]);
});

test('pane wait returns only bounded invalidation metadata and releases its subscription', async () => {
  const { api } = fake();
  let listener;
  let disposed = 0;
  api.watchPaneChanges = async (next) => { listener = next; return async () => { disposed += 1; }; };
  const wait = tools(api).find(({ name }) => name === 'husklet_pane_wait');
  const pending = wait.run({ slot: 'pane-2', timeout_ms: 1000 });
  await new Promise((resolve) => setImmediate(resolve));
  listener({ slot: 'pane-1', kind: 'terminal', revision: 0, generation: 1, coalesced: 0 });
  listener({ slot: 'pane-2', kind: 'native', revision: 8, generation: 2, coalesced: 6 });
  const answer = await pending;
  assert.deepEqual(JSON.parse(answer.content[0].text), {
    changed: true,
    change: { slot: 'pane-2', kind: 'native', revision: 8, generation: 2, coalesced: 6 },
  });
  assert.equal(disposed, 1);
  assert(!answer.content[0].text.includes('lines'));
  assert(!answer.content[0].text.includes('value'));
});

test('semantic XML escapes every XML metacharacter and remains structurally bounded', () => {
  const hostile = `&<>"'`;
  assert.equal(semanticXml({ slot: hostile, revision: 3, truncated: false, root: {
    id: hostile, role: hostile, label: hostile, value: '[redacted]', disabled: true, destructive: false,
    actions: [hostile], children: [],
  }}), '<pane slot="&amp;&lt;&gt;&quot;&apos;" revision="3" truncated="false"><node id="&amp;&lt;&gt;&quot;&apos;" role="&amp;&lt;&gt;&quot;&apos;" disabled="true" destructive="false" actions="&amp;&lt;&gt;&quot;&apos;"><label>&amp;&lt;&gt;&quot;&apos;</label><value>[redacted]</value></node></pane>');
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
  const events = new Set();
  const session = {
    onEvent: (listener) => { events.add(listener); return () => events.delete(listener); },
    call: async (name, argument) => {
      calls.push([name, argument]);
      if (name === 'workspace_info') return { reply: 'workspace', with: { name: 'demo' } };
      if (name === 'execution_inspect') return { reply: 'execution', with: {
        id: argument.id, container_id: 'container-1', running: true, exit_code: null,
      } };
      if (name === 'image_list') return { reply: 'images', with: [{ id: 'sha256:abc', references: ['alpine:3.20'], size: 123 }] };
      if (name === 'volume_list') return { reply: 'volumes', with: [{ name: 'cache', driver: 'local' }] };
      if (name === 'network_list') return { reply: 'networks', with: [{ id: 'n1', name: 'private', driver: 'bridge' }] };
      if (name === 'pane_semantic_read') return { reply: 'semantics', with: {
        slot: argument.slot, revision: 11, truncated: false,
        root: { id: 0, role: 'column', label: 'Live', value: null, disabled: false, actions: [], children: [] },
      } };
      if (name === 'pane_semantic_action') return { reply: 'done' };
      if (name === 'event_subscribe') {
        queueMicrotask(() => { for (const listener of events) listener({ snapshot: 'pane_changes', of: {
          slot: 'pane-live', kind: 'surface', revision: 12, generation: 13, coalesced: 2,
        }}); });
        return { reply: 'done' };
      }
      if (name === 'event_unsubscribe') return { reply: 'done' };
      throw new Error(`unexpected call ${name}`);
    },
  };
  const server = createServer(session);
  const client = new Client({ name: 'test', version: '1' });
  const [clientTransport, serverTransport] = InMemoryTransport.createLinkedPair();
  await Promise.all([server.connect(serverTransport), client.connect(clientTransport)]);
  const listed = await client.listTools();
  assert(listed.tools.some(({ name }) => name === 'husklet_workspace_info'));
  assert(listed.tools.some(({ name }) => name === 'husklet_container_execution'));
  assert(listed.tools.some(({ name }) => name === 'husklet_image_list'));
  assert(listed.tools.some(({ name }) => name === 'husklet_volume_list'));
  assert(listed.tools.some(({ name }) => name === 'husklet_network_list'));
  assert(listed.tools.some(({ name }) => name === 'husklet_pane_snapshot'));
  assert(listed.tools.some(({ name }) => name === 'husklet_pane_read'));
  assert(listed.tools.some(({ name }) => name === 'husklet_pane_action'));
  assert(listed.tools.some(({ name }) => name === 'husklet_pane_wait'));
  const answer = await client.callTool({ name: 'husklet_workspace_info', arguments: {} });
  assert.equal(answer.content[0].text, '{"name":"demo"}');
  const execution = await client.callTool({ name: 'husklet_container_execution', arguments: { id: 'exec-live' } });
  assert.deepEqual(JSON.parse(execution.content[0].text), {
    id: 'exec-live', container_id: 'container-1', running: true, exit_code: null,
  });
  const images = await client.callTool({ name: 'husklet_image_list', arguments: {} });
  assert.deepEqual(JSON.parse(images.content[0].text), [{ id: 'sha256:abc', references: ['alpine:3.20'], size: 123 }]);
  const volumes = await client.callTool({ name: 'husklet_volume_list', arguments: {} });
  assert.deepEqual(JSON.parse(volumes.content[0].text), [{ name: 'cache', driver: 'local' }]);
  const networks = await client.callTool({ name: 'husklet_network_list', arguments: {} });
  assert.deepEqual(JSON.parse(networks.content[0].text), [{ id: 'n1', name: 'private', driver: 'bridge' }]);
  const snapshot = await client.callTool({ name: 'husklet_pane_snapshot', arguments: { slot: 'pane-live' } });
  assert.match(snapshot.content[0].text, /^<pane slot="pane-live" revision="11"/);
  await client.callTool({ name: 'husklet_pane_action', arguments: { slot: 'pane-live', revision: 11, node: 0, action: 'invoke' } });
  const waited = await client.callTool({ name: 'husklet_pane_wait', arguments: { slot: 'pane-live', timeout_ms: 1000 } });
  assert.deepEqual(JSON.parse(waited.content[0].text).change, {
    slot: 'pane-live', kind: 'surface', revision: 12, generation: 13, coalesced: 2,
  });
  assert.deepEqual(calls, [
    ['workspace_info', undefined],
    ['execution_inspect', { id: 'exec-live' }],
    ['image_list', undefined],
    ['volume_list', undefined],
    ['network_list', undefined],
    ['pane_semantic_read', { slot: 'pane-live' }],
    ['pane_semantic_read', { slot: 'pane-live' }],
    ['pane_semantic_action', { slot: 'pane-live', action: { revision: 11, node: 0, action: 'invoke' } }],
    ['event_subscribe', { topic: 'pane-changes' }],
    ['event_unsubscribe', { topic: 'pane-changes' }],
  ]);
  await client.close();
  await server.close();
});

test('real MCP transport returns packed XML for terminal and surface occupants', async () => {
  const calls = [];
  const pane = (slot, occupant) => ({ kind: 'pane', focused: slot === 'term', grid: occupant === 'terminal' ? { columns: 80, rows: 24 } : null,
    pane: { slot, occupant, working_directory: occupant === 'terminal' ? '/tmp' : null, command: occupant === 'terminal' ? 'sh' : null, provider: null } });
  const session = { call: async (name, argument) => {
    calls.push([name, argument]);
    if (name === 'terminal_topology') return { reply: 'topology', with: { active_tab: 'tab', tabs: [{ id: 'tab', title: 'Packed', root: {
      kind: 'split', division: 'beside', ratio_per_mille: 500, first: pane('term', 'terminal'), second: pane('surface', 'surface'),
    } }] } };
    if (name === 'terminal_read_pane') return { reply: 'text', with: { slot: argument.slot, lines: ['hello & goodbye'], truncated: false } };
    if (name === 'pane_semantic_read') return { reply: 'semantics', with: { slot: argument.slot, revision: 9, truncated: false,
      root: { id: 1, role: 'button', label: 'Deploy <now>', value: null, disabled: false, actions: ['invoke'], children: [] } } };
    throw new Error(`unexpected call ${name}`);
  } };
  const server = createServer(session);
  const client = new Client({ name: 'packed-consumer', version: '1' });
  const [clientTransport, serverTransport] = InMemoryTransport.createLinkedPair();
  await Promise.all([server.connect(serverTransport), client.connect(clientTransport)]);
  const terminal = await client.callTool({ name: 'husklet_pane_read', arguments: { slot: 'term', lines: 25 } });
  const surface = await client.callTool({ name: 'husklet_pane_read', arguments: { slot: 'surface' } });
  assert.match(terminal.content[0].text, /occupant="terminal".*hello &amp; goodbye/s);
  assert.match(surface.content[0].text, /occupant="surface".*Deploy &lt;now&gt;/s);
  assert.equal((terminal.content[0].text.match(/<husklet-pane /g) ?? []).length, 1);
  assert.equal((surface.content[0].text.match(/<husklet-pane /g) ?? []).length, 1);
  assert.deepEqual(calls.map(([name]) => name), [
    'terminal_topology', 'terminal_read_pane', 'terminal_topology', 'pane_semantic_read',
  ]);
  await client.close();
  await server.close();
});

import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import test from 'node:test';
import { Client } from '@modelcontextprotocol/sdk/client/index.js';
import { InMemoryTransport } from '@modelcontextprotocol/sdk/inMemory.js';
import { createServer, paneXml, semanticXml, tools } from '../src/index.js';
import { workspace } from '../../react/src/index.js';

function fake() {
  const calls = [];
  const record = (name, answer = { ok: true }) => async (...args) => { calls.push([name, ...args]); return answer; };
  return { calls, api: {
    info: record('info', { name: 'demo', token: 'never expose me' }), list: record('list'), inspect: record('inspect'), create: record('workspace.create'), adopt: record('workspace.adopt'), update: record('workspace.update'),
    start: record('workspace.start'), stop: record('workspace.stop'), restart: record('workspace.restart'), delete: record('workspace.delete'),
    extensions: { list: record('extensions.list'), inspect: record('extensions.inspect'), enable: record('extensions.enable'), disable: record('extensions.disable'), remove: record('extensions.remove'), startAcquisition: record('extensions.startAcquisition'), acquisition: record('extensions.acquisition'), cancelAcquisition: record('extensions.cancelAcquisition'), install: record('extensions.install'), update: record('extensions.update') },
    containers: { list: record('containers.list'), inspect: record('containers.inspect'), processes: record('containers.processes'), execution: record('containers.execution'), executions: record('containers.executions'), executionLogs: record('containers.executionLogs'), waitExecution: record('containers.waitExecution'), signalExecution: record('containers.signalExecution'), removeExecution: record('containers.removeExecution'), logs: record('containers.logs'), create: record('containers.create'), exec: record('containers.exec'), start: record('containers.start'), stop: record('containers.stop'), pause: record('containers.pause'), unpause: record('containers.unpause'), restart: record('containers.restart'), rename: record('containers.rename'), remove: record('containers.remove'), kill: record('containers.kill') },
    images: { list: record('images.list'), inspect: record('images.inspect'), pull: record('images.pull'), startPull: record('images.startPull', { job: '7' }), pullStatus: record('images.pullStatus', { job: '7', revision: 1, state: 'starting' }), cancelPull: record('images.cancelPull'), remove: record('images.remove'), prune: record('images.prune') },
    volumes: { list: record('volumes.list'), inspect: record('volumes.inspect'), create: record('volumes.create'), remove: record('volumes.remove') },
    networks: { list: record('networks.list'), inspect: record('networks.inspect'), create: record('networks.create'), remove: record('networks.remove'), connect: record('networks.connect'), disconnect: record('networks.disconnect') },
    terminal: { tabs: record('terminal.tabs'), topology: record('terminal.topology'), read: record('terminal.read'), writeInput: record('terminal.writeInput'), openTab: record('terminal.openTab'), split: record('terminal.split'), splitObserved: record('terminal.splitObserved'), spawn: record('terminal.spawn'), spawnObserved: record('terminal.spawnObserved'), focus: record('terminal.focus'), focusObserved: record('terminal.focusObserved'), retitle: record('terminal.retitle'), retitleObserved: record('terminal.retitleObserved'), resizeGrid: record('terminal.resizeGrid'), resizeGridObserved: record('terminal.resizeGridObserved'), ratio: record('terminal.ratio'), ratioObserved: record('terminal.ratioObserved'), close: record('terminal.close'), closeObserved: record('terminal.closeObserved'), switchOccupant: record('terminal.switchOccupant'), switchOccupantObserved: record('terminal.switchOccupantObserved') },
    files: { list: record('files.list'), stat: record('files.stat'), read: record('files.read'), write: record('files.write'), mkdir: record('files.mkdir'), rename: record('files.rename'), remove: record('files.remove') },
    watchExtensions: async () => async () => {}, watchExtensionAcquisitions: async () => async () => {}, watchImagePulls: async () => async () => {},
  }};
}

const configuration = () => ({
  name: 'dev', image: 'alpine:3.20', architecture: 'arm64', storage: null, shell: '/bin/sh -l',
  cpus: 4, memory_mb: 4096, environment: [['MODE', 'dev']],
  mounts: [{ host: '/source', container: '/workspace', read_only: true }], docker_socket: true,
  scrollback: 100000, vpn: null, execution_lifetime: 'persisted',
  terminal: { font_family: 'Mono', font_size: 13, foreground: '#fff', background: '#000', cursor_shape: 'block', cursor_blink: false },
});
const generation = '0123456789abcdef0123456789abcdef';

const slotMutations = [
  'husklet_terminal_write', 'husklet_terminal_write_bytes', 'husklet_terminal_split',
  'husklet_terminal_spawn', 'husklet_terminal_focus', 'husklet_terminal_retitle',
  'husklet_terminal_resize', 'husklet_terminal_ratio',
  'husklet_terminal_switch_occupant', 'husklet_terminal_close',
];

const confirmedAuthority = new Map([
  ['husklet_workspace_adopt', ['configuration']],
  ['husklet_workspace_update', ['generation']],
  ['husklet_workspace_delete', ['generation']],
  ['husklet_extension_enable', ['image_digest']],
  ['husklet_extension_disable', ['image_digest']],
  ['husklet_extension_remove', ['image_digest']],
  ['husklet_extension_acquire', ['reference']],
  ['husklet_extension_acquisition_cancel', ['job', 'revision']],
  ['husklet_extension_install', ['job', 'revision']],
  ['husklet_extension_update', ['job', 'revision']],
  ['husklet_execution_signal', ['id']],
  ['husklet_execution_remove', ['id']],
  ['husklet_container_stop', ['id']],
  ['husklet_container_remove', ['id']],
  ['husklet_container_kill', ['id']],
  ['husklet_volume_remove', ['generation']],
  ['husklet_network_remove', ['reference']],
  ['husklet_network_disconnect', ['reference', 'container']],
  ['husklet_image_remove', ['reference']],
  // Prune is a host-selected bulk operation; the protocol exposes no target-set revision.
  ['husklet_image_prune', []],
  // Removal is confined beneath a pinned workspace dirfd, but files have no stable generation.
  ['husklet_file_remove', []],
  ['husklet_terminal_close', ['generation', 'revision']],
]);

test('every confirmation-gated MCP authority is classified with its strongest observed identity', () => {
  const listed = tools(fake().api);
  const gated = listed.filter(({ inputSchema }) => inputSchema?.shape?.confirm);
  assert.deepEqual(gated.map(({ name }) => name).sort(), [...confirmedAuthority.keys()].sort(),
    'a confirmation-gated tool must be classified, and a classified authority must stay gated');
  for (const tool of gated) {
    assert.equal(tool.inputSchema.shape.confirm.safeParse(true).success, true, `${tool.name} refuses literal confirmation`);
    assert.equal(tool.inputSchema.shape.confirm.safeParse(false).success, false, `${tool.name} accepts false confirmation`);
    assert.equal(tool.inputSchema.shape.confirm.safeParse(undefined).success, false, `${tool.name} makes confirmation optional`);
    for (const field of confirmedAuthority.get(tool.name)) {
      assert.ok(tool.inputSchema.shape[field], `${tool.name} lacks observed authority field ${field}`);
    }
  }
});

test('every React method referenced by an advertised MCP handler exists on the real typed facade', () => {
  const api = workspace({ call: async () => { throw new Error('not invoked'); } });
  const sources = [
    ['index.js', api],
    ['panes.js', api.terminal],
  ];
  for (const [name, root] of sources) {
    const source = fs.readFileSync(path.resolve(import.meta.dirname, `../src/${name}`), 'utf8');
    const references = new Set([...source.matchAll(/\bapi((?:\.[A-Za-z_$][\w$]*)+)/g)].map((match) => match[1]));
    for (const reference of references) {
      const segments = reference.slice(1).split('.');
      let value = root;
      for (const segment of segments) value = value?.[segment];
      assert.ok(value != null && (typeof value === 'function' || typeof value === 'object'),
        `${name} advertises handler reference api${reference}, absent from @husklet/react`);
    }
  }
  for (const operation of ['create', 'start', 'stop', 'delete']) {
    assert.equal(typeof api[operation], 'function', `dynamic workspace mutation ${operation} is absent from @husklet/react`);
  }
  for (const operation of ['start', 'pause', 'unpause', 'restart']) {
    assert.equal(typeof api.containers[operation], 'function', `dynamic container mutation ${operation} is absent from @husklet/react`);
  }
});

test('every MCP slot-targeted terminal mutation requires the complete pane cursor', () => {
  const listed = tools(fake().api);
  const terminal = listed.filter(({ name }) => name.startsWith('husklet_terminal_'));
  const intentionallyCursorless = new Set([
    'husklet_terminal_tabs', 'husklet_terminal_topology', 'husklet_terminal_read',
    'husklet_terminal_open',
  ]);
  assert.deepEqual(
    terminal.map(({ name }) => name).filter((name) => !intentionallyCursorless.has(name)).sort(),
    [...slotMutations].sort(),
    'a new terminal tool must be classified as observation/open or cursor-bound mutation',
  );
  for (const name of slotMutations) {
    const schema = listed.find((tool) => tool.name === name).inputSchema;
    assert.ok(schema.shape.generation, `${name} lacks generation`);
    assert.ok(schema.shape.revision, `${name} lacks revision`);
  }
});

test('workspace create and confirmed update preserve the complete typed configuration', async () => {
  const { api, calls } = fake();
  const listed = tools(api);
  const create = listed.find(({ name }) => name === 'husklet_workspace_create');
  const update = listed.find(({ name }) => name === 'husklet_workspace_update');
  const value = configuration();
  assert.equal(create.inputSchema.safeParse({ configuration: value }).success, true);
  assert.equal(create.inputSchema.safeParse({ configuration: { ...value, architecture: 'mips' } }).success, false);
  assert.equal(create.inputSchema.safeParse({ configuration: { ...value, mounts: [{ host: 'relative', container: '/x', read_only: false }] } }).success, false);
  assert.equal(update.inputSchema.safeParse({ name: 'dev', configuration: value }).success, false);
  assert.equal(update.inputSchema.safeParse({ name: 'other', configuration: value, confirm: true }).success, false);
  await create.run({ configuration: value });
  await update.run({ name: 'dev', generation, configuration: value, confirm: true });
  assert.deepEqual(calls, [['workspace.create', value], ['workspace.update', 'dev', generation, value]]);
});

test('legacy workspace adoption requires the exact generation-less snapshot and confirmation', async () => {
  const { api, calls } = fake();
  const adopt = tools(api).find(({ name }) => name === 'husklet_workspace_adopt');
  const legacy = { ...configuration(), generation: '' };
  assert.equal(adopt.inputSchema.safeParse({ configuration: legacy }).success, false);
  assert.equal(adopt.inputSchema.safeParse({ configuration: { ...legacy, image: 'changed' }, confirm: true }).success, true);
  assert.equal(adopt.inputSchema.safeParse({ configuration: { ...legacy, generation }, confirm: true }).success, false);
  await adopt.run({ configuration: legacy, confirm: true });
  assert.deepEqual(calls, [['workspace.adopt', legacy]]);
});

test('live MCP transport carries workspace configuration and host authority failures', async () => {
  const value = configuration();
  const calls = [];
  const session = { call: async (name, argument) => {
    calls.push([name, argument]);
    if (name === 'workspace_create') return { reply: 'workspace_configuration', with: value };
    if (name === 'workspace_update') throw new Error('denied: workspace-control');
    throw new Error(`unexpected call ${name}`);
  } };
  const server = createServer(session);
  const client = new Client({ name: 'workspace-config-test', version: '1' });
  const [clientTransport, serverTransport] = InMemoryTransport.createLinkedPair();
  await Promise.all([server.connect(serverTransport), client.connect(clientTransport)]);
  const created = await client.callTool({ name: 'husklet_workspace_create', arguments: { configuration: value } });
  assert.deepEqual(JSON.parse(created.content[0].text), value);
  const denied = await client.callTool({
    name: 'husklet_workspace_update', arguments: { name: 'dev', generation, configuration: value, confirm: true },
  });
  assert.equal(denied.isError, true);
  assert.match(denied.content[0].text, /workspace-control/);
  assert.deepEqual(calls, [
    ['workspace_create', { configuration: value }],
    ['workspace_update', { name: 'dev', generation, configuration: value }],
  ]);
  await client.close();
  await server.close();
});

test('schemas are strict, controls map exactly, and terminal spawn accepts argv rather than shell text', async () => {
  const { api, calls } = fake();
  const listed = tools(api);
  assert(!listed.some(({ name }) => /shell/.test(name)));
  const spawn = listed.find(({ name }) => name === 'husklet_terminal_spawn');
  assert(spawn);
  assert.equal(spawn.inputSchema.safeParse({ slot: 'pane-1', command: 'echo unsafe' }).success, false);
  assert.equal(spawn.inputSchema.safeParse({ slot: 'pane-1', command: ['printf'] }).success, false);
  assert.equal(spawn.inputSchema.safeParse({ slot: 'pane-1', command: [] }).success, false);
  assert.equal(spawn.inputSchema.safeParse({ slot: 'pane-1', command: [''] }).success, false);
  assert.equal(spawn.inputSchema.safeParse({ slot: 'pane-1', command: ['x'.repeat(4097)] }).success, false);
  assert.equal(spawn.inputSchema.safeParse({ slot: 'pane-1', command: ['x'.repeat(513), ...Array(63).fill('x'.repeat(512))] }).success, false);
  await spawn.run({ slot: 'pane-1', generation: 7, revision: 11, command: ['printf', '%s\n', 'ready'] });
  const start = listed.find(({ name }) => name === 'husklet_container_start');
  assert.equal(start.inputSchema.safeParse({ id: 'abc', extra: true }).success, false);
  assert.equal(start.inputSchema.safeParse({ id: 'abc' }).success, false);
  const immutable = 'a'.repeat(64);
  await start.run({ id: immutable });
  assert.deepEqual(calls, [
    ['terminal.spawnObserved', 'pane-1', 7, 11, ['printf', '%s\n', 'ready']],
    ['containers.start', immutable],
  ]);
});

test('container termination requires confirmation before host authority is called', async () => {
  const { api, calls } = fake();
  const listed = tools(api);
  const stop = listed.find(({ name }) => name === 'husklet_container_stop');
  const remove = listed.find(({ name }) => name === 'husklet_container_remove');
  const kill = listed.find(({ name }) => name === 'husklet_container_kill');
  assert.equal(stop.inputSchema.safeParse({ id: 'abc' }).success, false);
  assert.equal(stop.inputSchema.safeParse({ id: 'abc', confirm: false }).success, false);
  assert.equal(stop.inputSchema.safeParse({ id: 'abc', confirm: true }).success, false);
  assert.equal(remove.inputSchema.safeParse({ id: 'friendly-name', confirm: true }).success, false);
  assert.equal(kill.inputSchema.safeParse({ id: 'abc', signal: 'SIGKILL' }).success, false);
  assert.equal(kill.inputSchema.safeParse({ id: '1', signal: 'SIGKILL', confirm: true }).success, false);
  assert.equal(kill.inputSchema.safeParse({ id: 'abc', signal: 'SIGKILL', confirm: true }).success, false);
  assert.equal(kill.inputSchema.safeParse({ id: 'abc', signal: 'x'.repeat(33), confirm: true }).success, false);
  assert.deepEqual(calls, [], 'schema refusal cannot call host authority');
  const immutable = 'a'.repeat(64);
  await stop.run({ id: immutable, confirm: true });
  await remove.run({ id: immutable, confirm: true });
  await kill.run({ id: immutable, signal: 'SIGKILL', confirm: true });
  assert.deepEqual(calls, [['containers.stop', immutable], ['containers.remove', immutable], ['containers.kill', immutable, 'SIGKILL']]);
});

test('container rename requires an immutable ID and the native bounded name grammar', async () => {
  const { api, calls } = fake();
  const rename = tools(api).find(({ name }) => name === 'husklet_container_rename');
  const immutable32 = 'a'.repeat(32);
  const immutable64 = 'b'.repeat(64);
  for (const value of [
    { id: 'friendly-name', name: 'worker' },
    { id: 'a'.repeat(12), name: 'worker' },
    { id: immutable64, name: '.worker' },
    { id: immutable64, name: 'worker/name' },
    { id: immutable64, name: 'x'.repeat(129) },
    { id: immutable64, name: 'worker', confirm: true },
  ]) assert.equal(rename.inputSchema.safeParse(value).success, false);
  assert.deepEqual(calls, [], 'schema refusal cannot call host authority');
  await rename.run({ id: immutable32, name: 'worker_2.prod' });
  await rename.run({ id: immutable64, name: 'worker-3' });
  assert.deepEqual(calls, [
    ['containers.rename', immutable32, 'worker_2.prod'],
    ['containers.rename', immutable64, 'worker-3'],
  ]);
});

test('pane list exposes bounded discovery metadata without requiring known slots', async () => {
  const { api, calls } = fake();
  api.terminal.panes = async () => ({ panes: [
    { slot: 'term-1', kind: 'terminal', provider: null, tab: 'tab-1', title: 'Shell', focused: true },
    { slot: 'surface-1', kind: 'surface', provider: { extension: 'demo', provider: 'main' }, tab: 'tab-1', title: 'Shell', focused: false },
    { slot: 'workspace', kind: 'native', provider: null, tab: null, title: 'Workspace', focused: false },
  ], truncated: false });
  const paneList = tools(api).find(({ name }) => name === 'husklet_pane_list');
  const answer = await paneList.run({});
  const inventory = JSON.parse(answer.content[0].text);
  assert.deepEqual(inventory.panes.map(({ slot, kind }) => [slot, kind]), [
    ['term-1', 'terminal'], ['surface-1', 'surface'], ['workspace', 'native'],
  ]);
  assert(!answer.content[0].text.includes('contents'));
});

test('extension inventory is bounded and every lifecycle mutation requires confirmation', async () => {
  const { api, calls } = fake();
  const listed = tools(api);
  const byName = (name) => listed.find((tool) => tool.name === name);
  assert.equal(byName('husklet_extension_inspect').inputSchema.safeParse({ name: '../escape' }).success, false);
  for (const action of ['enable', 'disable', 'remove']) {
    assert.equal(byName(`husklet_extension_${action}`).inputSchema.safeParse({ name: 'workspace-manager' }).success, false);
    const input = { name: 'workspace-manager', image_digest: `sha256:${'a'.repeat(64)}`, confirm: true };
    await byName(`husklet_extension_${action}`).run(input);
  }
  await byName('husklet_extension_list').run({});
  await byName('husklet_extension_inspect').run({ name: 'workspace-manager' });
  assert.deepEqual(calls, [
    ['extensions.enable', 'workspace-manager', `sha256:${'a'.repeat(64)}`], ['extensions.disable', 'workspace-manager', `sha256:${'a'.repeat(64)}`],
    ['extensions.remove', 'workspace-manager', `sha256:${'a'.repeat(64)}`], ['extensions.list'], ['extensions.inspect', 'workspace-manager'],
  ]);
});

test('extension acquisition is asynchronous, digest-observable, grant-bounded, and confirmed', async () => {
  const { api, calls } = fake(); const listed = tools(api); const byName = (name) => listed.find((tool) => tool.name === name);
  for (const name of ['husklet_extension_acquire', 'husklet_extension_acquisition_cancel', 'husklet_extension_install', 'husklet_extension_update']) {
    assert.equal(byName(name).inputSchema.safeParse(name.endsWith('acquire') ? { reference: 'example:1' } : { job: 'j', revision: 1, granted: [], confirm: false }).success, false);
  }
  assert.equal(byName('husklet_extension_install').inputSchema.safeParse({ job: 'j', revision: 1, granted: ['made-up'], confirm: true }).success, false);
  assert.equal(byName('husklet_extension_install').inputSchema.safeParse({ job: 'j', revision: 1, granted: ['container-attach'], confirm: true }).success, true);
  assert.equal(byName('husklet_extension_install').inputSchema.safeParse({ job: 'j', revision: Number.MAX_SAFE_INTEGER + 1, granted: [], confirm: true }).success, false);
  assert.equal(byName('husklet_extension_acquisition_cancel').inputSchema.safeParse({ job: 'j', confirm: true }).success, false);
  assert.equal(byName('husklet_extension_acquisition_cancel').inputSchema.safeParse({ job: 'j', revision: -1, confirm: true }).success, false);
  assert.equal(byName('husklet_extension_acquisition_cancel').inputSchema.safeParse({ job: 'j', revision: Number.MAX_SAFE_INTEGER + 1, confirm: true }).success, false);
  await byName('husklet_extension_acquire').run({ reference: 'example:1', confirm: true });
  await byName('husklet_extension_acquisition').run({ job: 'j' });
  await byName('husklet_extension_acquisition_cancel').run({ job: 'j', revision: 4, confirm: true });
  await byName('husklet_extension_install').run({ job: 'j', revision: 4, granted: ['interface', 'container-attach'], confirm: true });
  await byName('husklet_extension_update').run({ job: 'j', revision: 4, granted: ['interface'], confirm: true });
  assert.deepEqual(calls, [['extensions.startAcquisition', 'example:1'], ['extensions.acquisition', 'j'], ['extensions.cancelAcquisition', 'j', 4], ['extensions.install', 'j', 4, ['interface', 'container-attach']], ['extensions.update', 'j', 4, ['interface']]]);
});

test('extension wait filters acquisition jobs and disposes its credit-controlled watcher', async () => {
  const { api } = fake(); let listener; let disposed = 0;
  api.watchExtensionAcquisitions = async (next) => { listener = next; return async () => { disposed += 1; }; };
  const wait = tools(api).find(({ name }) => name === 'husklet_extension_wait');
  assert.equal(wait.inputSchema.safeParse({ kind: 'inventory', job: 'j', timeout_ms: 10 }).success, false);
  assert.equal(wait.inputSchema.safeParse({ kind: 'inventory', timeout_ms: 10 }).success, false);
  assert.equal(wait.inputSchema.safeParse({ kind: 'inventory', after: { name: 'manager', image_digest: `sha256:${'a'.repeat(64)}`, status: 'made-up' }, timeout_ms: 10 }).success, false);
  assert.equal(wait.inputSchema.safeParse({ kind: 'acquisition', job: 'wanted', timeout_ms: 10 }).success, false);
  assert.equal(wait.inputSchema.safeParse({ kind: 'acquisition', after_revision: 1, timeout_ms: 10 }).success, false);
  const pending = wait.run({ kind: 'acquisition', job: 'wanted', after_revision: 2, timeout_ms: 1000 });
  await new Promise((resolve) => setImmediate(resolve));
  listener({ job: 'other', revision: 1, state: 'ready', coalesced: 0 });
  listener({ job: 'wanted', revision: 2, state: 'ready', coalesced: 3 });
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(disposed, 0, 'the already-observed revision must not settle or dispose the wait');
  listener({ job: 'wanted', revision: 3, state: 'committing', coalesced: 0 });
  const answer = await pending;
  assert.deepEqual(JSON.parse(answer.content[0].text), { changed: true, change: { job: 'wanted', revision: 3, state: 'committing', coalesced: 0 } });
  assert.equal(disposed, 1);
});

test('installed extension wait ignores its initial tuple and distinguishes status, removal, and replacement', async () => {
  const { api } = fake(); const listeners = new Set(); let disposed = 0;
  api.watchExtensions = async (next) => {
    listeners.add(next);
    return async () => { if (listeners.delete(next)) disposed += 1; };
  };
  const wait = tools(api).find(({ name }) => name === 'husklet_extension_wait');
  const digest = `sha256:${'a'.repeat(64)}`; const replacement = `sha256:${'b'.repeat(64)}`;
  const after = { name: 'manager', image_digest: digest, status: 'standby' };
  const status = wait.run({ kind: 'inventory', after, timeout_ms: 1000 });
  const removed = wait.run({ kind: 'inventory', after: { ...after, name: 'removed' }, timeout_ms: 1000 });
  const replaced = wait.run({ kind: 'inventory', after: { ...after, name: 'replacement' }, timeout_ms: 1000 });
  await new Promise((resolve) => setImmediate(resolve));
  for (const listener of [...listeners]) listener([after, { ...after, name: 'removed' }, { ...after, name: 'replacement' }]);
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(disposed, 0, 'the unchanged initial inventory must not settle any wait');
  for (const listener of [...listeners]) listener([
    { ...after, status: 'duty' },
    { ...after, name: 'replacement', image_digest: replacement },
  ]);
  const statusAnswer = JSON.parse((await status).content[0].text);
  const removedAnswer = JSON.parse((await removed).content[0].text);
  const replacedAnswer = JSON.parse((await replaced).content[0].text);
  assert.deepEqual(statusAnswer.extension, { ...after, status: 'duty' });
  assert.equal(statusAnswer.removed, false); assert.equal(statusAnswer.replaced, false);
  assert.equal(removedAnswer.extension, null); assert.equal(removedAnswer.removed, true);
  assert.equal(replacedAnswer.extension.image_digest, replacement); assert.equal(replacedAnswer.replaced, true);
  assert.equal(disposed, 3); assert.equal(listeners.size, 0);
});

test('concurrent extension waits advance independently from their exact observed revisions', async () => {
  const { api } = fake(); const listeners = new Set(); let disposed = 0;
  api.watchExtensionAcquisitions = async (next) => {
    listeners.add(next);
    return async () => { if (listeners.delete(next)) disposed += 1; };
  };
  const wait = tools(api).find(({ name }) => name === 'husklet_extension_wait');
  const first = wait.run({ kind: 'acquisition', job: 'wanted', after_revision: 1, timeout_ms: 1000 });
  const second = wait.run({ kind: 'acquisition', job: 'wanted', after_revision: 2, timeout_ms: 1000 });
  await new Promise((resolve) => setImmediate(resolve));
  for (const listener of [...listeners]) listener({ job: 'wanted', revision: 2, state: 'ready', coalesced: 0 });
  assert.equal(JSON.parse((await first).content[0].text).change.revision, 2);
  assert.equal(disposed, 1);
  for (const listener of [...listeners]) listener({ job: 'wanted', revision: 3, state: 'committing', coalesced: 0 });
  assert.equal(JSON.parse((await second).content[0].text).change.revision, 3);
  assert.equal(disposed, 2);
  assert.equal(listeners.size, 0);
});

test('container create and exec accept only bounded structured authority', async () => {
  const { api, calls } = fake();
  const listed = tools(api);
  const create = listed.find(({ name }) => name === 'husklet_container_create');
  const exec = listed.find(({ name }) => name === 'husklet_container_exec');
  const attach = listed.find(({ name }) => name === 'husklet_container_attach_terminal');
  assert.equal(create.inputSchema.safeParse({ image: 'alpine:3.20', name: 'worker-1' }).success, true);
  assert.equal(create.inputSchema.safeParse({ image: 'alpine:3.20', name: 'worker-1', hostname: 'h'.repeat(253) }).success, true);
  assert.equal(create.inputSchema.safeParse({ image: 'alpine:3.20', name: 'worker-1', hostname: 'h'.repeat(254) }).success, false);
  assert.equal(create.inputSchema.safeParse({ image: 'alpine:3.20', name: 'worker-1', hostname: 'bad\nname' }).success, false);
  assert.equal(create.inputSchema.safeParse({ image: 'é'.repeat(256), name: 'worker-1' }).success, true);
  assert.equal(create.inputSchema.safeParse({ image: '😀'.repeat(129), name: 'worker-1' }).success, false);
  assert.equal(create.inputSchema.safeParse({ image: 'alpine:3.20', name: 'worker-1', user: 'é'.repeat(128) }).success, true);
  assert.equal(create.inputSchema.safeParse({ image: 'alpine:3.20', name: 'worker-1', user: '😀'.repeat(65) }).success, false);
  assert.equal(create.inputSchema.safeParse({ image: 'alpine:3.20', name: 'worker-1', environment: [['VALUE', 'é'.repeat(4096)]] }).success, true);
  assert.equal(create.inputSchema.safeParse({ image: 'alpine:3.20', name: 'worker-1', environment: [['VALUE', '😀'.repeat(2049)]] }).success, false);
  assert.equal(create.inputSchema.safeParse({ image: 'alpine:3.20', name: 'worker-1', environment: [['é'.repeat(128), 'value']] }).success, true);
  assert.equal(create.inputSchema.safeParse({ image: 'alpine:3.20', name: 'worker-1', environment: [['é'.repeat(129), 'value']] }).success, false);
  assert.equal(create.inputSchema.safeParse({ image: 'alpine:3.20', name: 'worker-1', environment: [['release-name', 'value']] }).success, true);
  assert.equal(create.inputSchema.safeParse({ image: 'alpine:3.20', name: 'worker-1', environment: [['BAD=NAME', 'value']] }).success, false);
  assert.equal(create.inputSchema.safeParse({ image: 'alpine:3.20', name: 'worker-1', labels: [['note', 'é'.repeat(2048)]] }).success, true);
  assert.equal(create.inputSchema.safeParse({ image: 'alpine:3.20', name: 'worker-1', labels: [['note', '😀'.repeat(1025)]] }).success, false);
  assert.equal(create.inputSchema.safeParse({ image: 'alpine:3.20', name: 'worker-1', labels: [['é'.repeat(128), 'note']] }).success, true);
  assert.equal(create.inputSchema.safeParse({ image: 'alpine:3.20', name: 'worker-1', labels: [['😀'.repeat(65), 'note']] }).success, false);
  assert.equal(create.inputSchema.safeParse({ image: 'alpine latest', name: 'worker' }).success, false);
  assert.equal(create.inputSchema.safeParse({ image: 'alpine:3.20', name: '../worker' }).success, false);
  assert.equal(create.inputSchema.safeParse({ image: 'alpine:3.20', name: 'worker', mounts: [{ volume: 'cache', target: '../host', read_only: false }] }).success, false);
  const exactTarget = `/${'é'.repeat(2047)}a`;
  const oversizedTarget = `/${'😀'.repeat(1024)}a`;
  assert.equal(create.inputSchema.safeParse({ image: 'alpine:3.20', name: 'worker', mounts: [{ volume: 'cache', target: exactTarget }] }).success, true);
  assert.equal(create.inputSchema.safeParse({ image: 'alpine:3.20', name: 'worker', mounts: [{ volume: 'cache', target: oversizedTarget }] }).success, false);
  assert.equal(create.inputSchema.safeParse({ image: 'alpine:3.20', name: 'worker', mounts: [{ volume: 'v'.repeat(255), target: '/data' }], network: 'n'.repeat(255) }).success, true);
  assert.equal(create.inputSchema.safeParse({ image: 'alpine:3.20', name: 'worker', mounts: [{ volume: 'v'.repeat(256), target: '/data' }] }).success, false);
  assert.equal(create.inputSchema.safeParse({ image: 'alpine:3.20', name: 'worker', network: '-invalid' }).success, false);
  assert.equal(create.inputSchema.safeParse({ image: 'alpine:3.20', name: 'worker', ports: [{ container: 80, host: 0, protocol: 'tcp' }] }).success, false);
  const containerId = 'a'.repeat(64);
  assert.equal(exec.inputSchema.safeParse({ id: 'c1', command: ['true'] }).success, false);
  assert.equal(exec.inputSchema.safeParse({ id: containerId, command: 'sh -lc whoami' }).success, false);
  assert.equal(exec.inputSchema.safeParse({ id: containerId, command: [] }).success, false);
  assert.equal(exec.inputSchema.safeParse({ id: containerId, command: Array(65).fill('x') }).success, false);
  assert.equal(exec.inputSchema.safeParse({ id: containerId, command: ['printf', '😀'.repeat(1025)] }).success, false);
  assert.equal(exec.inputSchema.safeParse({ id: containerId, command: ['true'], working_directory: 'relative' }).success, false);
  assert.equal(attach.inputSchema.safeParse({ id: 'c1', command: ['sh'] }).success, false);
  assert.equal(attach.inputSchema.safeParse({ id: 'a'.repeat(64), command: ['sh', '-i'] }).success, true);
  assert.equal(attach.inputSchema.safeParse({ id: 'a'.repeat(64), command: ['printf', 'é'] }).success, true);
  assert.equal(attach.inputSchema.safeParse({ id: 'a'.repeat(64), command: ['printf', '😀'.repeat(1025)] }).success, false);
  assert.equal(attach.inputSchema.safeParse({ id: 'a'.repeat(64), command: 'sh -i' }).success, false);
  const spec = create.inputSchema.parse({
    image: 'alpine:3.20', name: 'worker-1', entrypoint: ['/usr/bin/env'], command: ['worker', '--once'],
    environment: [['MODE', 'agent']], working_directory: '/work', user: '1000', labels: [['owner', 'agent']],
    mounts: [{ volume: 'cache', target: '/cache', read_only: false }], network: 'private',
    ports: [{ container: 8080, host: 18080, protocol: 'tcp' }], memory_mb: 512, cpus: 2, pids_limit: 128,
  });
  api.containers.create = async (...args) => { calls.push(['containers.create', ...args]); return 'c-created'; };
  api.containers.exec = async (...args) => { calls.push(['containers.exec', ...args]); return 'e-created'; };
  assert.deepEqual(JSON.parse((await create.run(spec)).content[0].text), { id: 'c-created' });
  assert.deepEqual(JSON.parse((await exec.run({ id: containerId, command: ['printf', '%s', 'hello'], user: '1000', working_directory: '/work' })).content[0].text), { id: 'e-created' });
  assert.deepEqual(calls, [
    ['containers.create', spec],
    ['containers.exec', containerId, { command: ['printf', '%s', 'hello'], user: '1000', workingDirectory: '/work' }],
  ]);
});

test('filesystem controls are strict and removal requires explicit confirmation', async () => {
  const { api, calls } = fake();
  const listed = tools(api);
  const byName = (name) => listed.find((tool) => tool.name === name);
  assert.equal(byName('husklet_file_remove').inputSchema.safeParse({ path: 'old.txt' }).success, false);
  assert.equal(byName('husklet_file_rename').inputSchema.safeParse({ from: 'a', to: 'b', extra: true }).success, false);
  assert.equal(byName('husklet_file_write').inputSchema.safeParse({ path: 'full.txt', contents: 'é'.repeat(32_768) }).success, true);
  assert.equal(byName('husklet_file_write').inputSchema.safeParse({ path: 'large.txt', contents: '😀'.repeat(16_385) }).success, false);
  assert.equal(byName('husklet_file_read').inputSchema.safeParse({ path: 'é'.repeat(2048) }).success, true);
  assert.equal(byName('husklet_file_read').inputSchema.safeParse({ path: '😀'.repeat(1025) }).success, false);
  await byName('husklet_file_stat').run({ path: 'logs/app.log' });
  await byName('husklet_file_mkdir').run({ path: 'logs/new' });
  await byName('husklet_file_rename').run({ from: 'logs/a', to: 'logs/b' });
  await byName('husklet_file_remove').run({ path: 'logs/b', confirm: true });
  assert.deepEqual(calls, [['files.stat', 'logs/app.log'], ['files.mkdir', 'logs/new'], ['files.rename', 'logs/a', 'logs/b'], ['files.remove', 'logs/b']]);
});

test('container execution inspection is a strict bounded read through the typed API', async () => {
  const { api, calls } = fake();
  const execution = tools(api).find(({ name }) => name === 'husklet_container_execution');
  assert(execution);
  assert.equal(execution.inputSchema.safeParse({ id: 'exec-1', extra: true }).success, false);
  assert.equal(execution.inputSchema.safeParse({ id: 'exec-1' }).success, false);
  const immutable = 'e'.repeat(32);
  await execution.run({ id: immutable });
  assert.deepEqual(calls, [['containers.execution', immutable]]);
});

test('process inspection exposes its finite initial-process scope and snapshot PID identity', async () => {
  const { api, calls } = fake();
  const snapshot = { container_id: 'c'.repeat(64), titles: ['PID', 'PPID', 'USER', 'STAT', 'COMMAND'],
    processes: [['1', '0', 'root', '?', '/usr/bin/server']], observed_at_ms: 1_700_000_000_000,
    scope: 'initial', pid_identity: 'snapshot', truncated: false };
  api.containers.processes = async (...args) => { calls.push(['containers.processes', ...args]); return snapshot; };
  const processTool = tools(api).find(({ name }) => name === 'husklet_container_processes');
  const result = await processTool.run({ id: 'c1' });
  assert.deepEqual(JSON.parse(result.content[0].text), snapshot);
  assert.deepEqual(calls, [['containers.processes', 'c1']]);
});

test('execution wait is a strict bounded read and preserves the timeout', async () => {
  const { api, calls } = fake();
  const wait = tools(api).find(({ name }) => name === 'husklet_execution_wait');
  const execution = 'e'.repeat(32);
  assert.equal(wait.inputSchema.safeParse({ id: 'e1', timeout_ms: 0 }).success, false);
  assert.equal(wait.inputSchema.safeParse({ id: 'e1', timeout_ms: 30_001 }).success, false);
  assert.equal(wait.inputSchema.safeParse({ id: 'e1', timeout_ms: 10, extra: true }).success, false);
  await wait.run({ id: execution, timeout_ms: 1250 });
  assert.deepEqual(calls, [['containers.waitExecution', execution, { timeoutMs: 1250 }]]);
});

test('execution catalogue and output are finite strict reads', async () => {
  const { api, calls } = fake();
  const output = { stdout: [111], stderr: [], truncated: true, stdout_truncated: true,
    stderr_truncated: false, eof: false };
  api.containers.executionLogs = async (...args) => { calls.push(['containers.executionLogs', ...args]); return output; };
  const listed = tools(api);
  const list = listed.find(({ name }) => name === 'husklet_execution_list');
  const logs = listed.find(({ name }) => name === 'husklet_execution_logs');
  const execution = 'e'.repeat(32);
  assert.equal(logs.inputSchema.safeParse({ id: 'e1', stdout: false, stderr: false }).success, false);
  assert.equal(logs.inputSchema.safeParse({ id: 'e1', extra: true }).success, false);
  await list.run({});
  const result = await logs.run({ id: execution, stdout: true, stderr: false });
  assert.deepEqual(JSON.parse(result.content[0].text), output);
  assert.deepEqual(calls, [['containers.executions'], ['containers.executionLogs', execution, { stdout: true, stderr: false }]]);
});

test('execution signaling targets an execution with a strict bounded signal and confirmation', async () => {
  const { api, calls } = fake();
  const signal = tools(api).find(({ name }) => name === 'husklet_execution_signal');
  const immutable = 'b'.repeat(32);
  assert.equal(signal.inputSchema.safeParse({ id: 'e1', signal: '' }).success, false);
  assert.equal(signal.inputSchema.safeParse({ id: '1', signal: 'SIGTERM' }).success, false);
  assert.equal(signal.inputSchema.safeParse({ id: 'friendly', signal: 'SIGTERM' }).success, false);
  assert.equal(signal.inputSchema.safeParse({ id: 'e1', signal: 'x'.repeat(33) }).success, false);
  assert.equal(signal.inputSchema.safeParse({ id: immutable, signal: '😀'.repeat(9) }).success, false);
  assert.equal(signal.inputSchema.safeParse({ id: immutable, signal: 'SIGTERM' }).success, false);
  assert.equal(signal.inputSchema.safeParse({ id: immutable, signal: 'SIGTERM', confirm: false }).success, false);
  assert.equal(calls.length, 0, 'missing confirmation cannot reach execution authority');
  await signal.run({ id: immutable, signal: 'SIGTERM', confirm: true });
  assert.deepEqual(calls, [['containers.signalExecution', immutable, 'SIGTERM']]);
});

test('execution removal requires literal confirmation', async () => {
  const { api, calls } = fake();
  const remove = tools(api).find(({ name }) => name === 'husklet_execution_remove');
  const immutable = 'e'.repeat(32);
  assert.equal(remove.inputSchema.safeParse({ id: 'e1' }).success, false);
  assert.equal(remove.inputSchema.safeParse({ id: 'e1', confirm: false }).success, false);
  assert.equal(remove.inputSchema.safeParse({ id: 'e1', confirm: true }).success, false);
  assert.equal(remove.inputSchema.safeParse({ id: immutable, confirm: false }).success, false);
  await remove.run({ id: immutable, confirm: true });
  assert.deepEqual(calls, [['containers.removeExecution', immutable]]);
});

test('terminal layout tools use the host wire vocabulary and bounded destructive controls', async () => {
  const { api, calls } = fake();
  const listed = tools(api);
  const open = listed.find(({ name }) => name === 'husklet_terminal_open');
  const split = listed.find(({ name }) => name === 'husklet_terminal_split');
  const resize = listed.find(({ name }) => name === 'husklet_terminal_resize');
  const ratio = listed.find(({ name }) => name === 'husklet_terminal_ratio');
  const retitle = listed.find(({ name }) => name === 'husklet_terminal_retitle');
  const focus = listed.find(({ name }) => name === 'husklet_terminal_focus');
  const close = listed.find(({ name }) => name === 'husklet_terminal_close');
  assert.equal(split.inputSchema.safeParse({ slot: 'pane-1', division: 'horizontal' }).success, false);
  assert.equal(split.inputSchema.safeParse({ slot: 'pane-1', division: 'beside' }).success, false);
  assert.equal(split.inputSchema.safeParse({ slot: 'pane-1', generation: 2, revision: 3, division: 'beside' }).success, true);
  assert.equal(resize.inputSchema.safeParse({ slot: 'pane-1', columns: 0, rows: 24 }).success, false);
  assert.equal(ratio.inputSchema.safeParse({ slot: 'pane-1', ratio: 0.99 }).success, false);
  assert.equal(ratio.inputSchema.safeParse({ slot: 'pane-1', generation: 2, revision: 3, ratio: 0.6 }).success, true);
  assert.equal(close.inputSchema.safeParse({ slot: 'pane-1' }).success, false);
  assert.equal(close.inputSchema.safeParse({ slot: 'pane-1', generation: 2, revision: 3, confirm: true }).success, true);
  for (const title of ['', '   ', 'line\nbreak', 'nul\0byte', '🧪'.repeat(65)]) {
    assert.equal(retitle.inputSchema.safeParse({ slot: 'pane-1', title }).success, false);
  }
  await open.run({});
  await split.run({ slot: 'pane-1', generation: 2, revision: 3, division: 'below' });
  await resize.run({ slot: 'pane-1', generation: 2, revision: 3, columns: 120, rows: 40 });
  await ratio.run({ slot: 'pane-1', generation: 2, revision: 3, ratio: 0.6 });
  await focus.run({ slot: 'pane-1', generation: 2, revision: 3 });
  await retitle.run({ slot: 'pane-1', generation: 2, revision: 3, title: ' Build 🧪 ' });
  await close.run({ slot: 'pane-1', generation: 2, revision: 3, confirm: true });
  assert.deepEqual(calls, [
    ['terminal.openTab', 'Terminal'],
    ['terminal.splitObserved', 'pane-1', 2, 3, 'below'],
    ['terminal.resizeGridObserved', 'pane-1', 2, 3, 120, 40],
    ['terminal.ratioObserved', 'pane-1', 2, 3, 0.6],
    ['terminal.focusObserved', 'pane-1', 2, 3],
    ['terminal.retitleObserved', 'pane-1', 2, 3, ' Build 🧪 '],
    ['terminal.closeObserved', 'pane-1', 2, 3],
  ]);
});

test('terminal occupant switching exposes a strict snapshot-safe target', async () => {
  const { api, calls } = fake();
  const tool = tools(api).find(({ name }) => name === 'husklet_terminal_switch_occupant');
  assert.equal(tool.inputSchema.safeParse({ slot: 'pane-1', generation: -1, revision: 0, target: { kind: 'terminal' } }).success, false);
  assert.equal(tool.inputSchema.safeParse({ slot: 'pane-1', generation: 0, target: { kind: 'terminal' } }).success, false);
  assert.equal(tool.inputSchema.safeParse({ slot: 'pane-1', generation: 0, target: { kind: 'terminal', extra: true } }).success, false);
  assert.equal(tool.inputSchema.safeParse({ slot: 'pane-1', generation: 0, target: { kind: 'surface', extension: 'demo' } }).success, false);
  await tool.run({ slot: 'pane-1', generation: 3, revision: 8, target: { kind: 'surface', extension: 'demo', provider: 'main' } });
  await tool.run({ slot: 'pane-1', generation: 4, revision: 9, target: { kind: 'terminal' } });
  assert.deepEqual(calls, [
    ['terminal.switchOccupantObserved', 'pane-1', 3, 8, { kind: 'surface', extension: 'demo', provider: 'main' }],
    ['terminal.switchOccupantObserved', 'pane-1', 4, 9, { kind: 'terminal' }],
  ]);
});

test('terminal byte input decodes canonical base64 exactly and refuses ambiguity or overflow before calling', async () => {
  const { api, calls } = fake();
  const write = tools(api).find(({ name }) => name === 'husklet_terminal_write_bytes');
  const exact = Uint8Array.from([0x00, 0x03, 0x1b, 0x7f, 0x80, 0xff]);
  const encoded = Buffer.from(exact).toString('base64');
  assert.equal(write.inputSchema.safeParse({ slot: 'pane-1', generation: 3, revision: 4, input_base64: encoded }).success, true);
  for (const invalid of ['AA', 'AA==\n', 'AA', 'AA-_', 'AB==']) {
    assert.equal(write.inputSchema.safeParse({ slot: 'pane-1', generation: 3, revision: 4, input_base64: invalid }).success, false, invalid);
  }
  const oversized = Buffer.alloc(65_537).toString('base64');
  assert.equal(write.inputSchema.safeParse({ slot: 'pane-1', generation: 3, revision: 4, input_base64: oversized }).success, false);
  await assert.rejects(write.run({ slot: 'pane-1', generation: 3, revision: 4, input_base64: oversized }), /exceeds 65536 bytes/);
  assert.deepEqual(calls, []);
  await write.run({ slot: 'pane-1', generation: 3, revision: 4, input_base64: encoded });
  assert.deepEqual(calls, [['terminal.writeInput', 'pane-1', 3, 4, exact]]);
});

test('terminal text input exposes the complete host byte allowance', async () => {
  const { api, calls } = fake();
  const write = tools(api).find(({ name }) => name === 'husklet_terminal_write');
  const exact = 'é'.repeat(32_768);
  assert.equal(write.inputSchema.safeParse({ slot: 'pane-1', generation: 3, revision: 4, input: exact }).success, true);
  assert.equal(write.inputSchema.safeParse({ slot: 'pane-1', generation: 3, revision: 4, input: '😀'.repeat(16_385) }).success, false);
  assert.equal(write.inputSchema.safeParse({ slot: 'pane-1', input: exact }).success, false);
  await write.run({ slot: 'pane-1', generation: 3, revision: 4, input: exact });
  assert.deepEqual(calls, [['terminal.writeInput', 'pane-1', 3, 4, exact]]);
});

test('image tools use typed reads and require confirmation for destructive controls', async () => {
  const { api, calls } = fake();
  const listed = tools(api);
  const byName = (name) => listed.find((tool) => tool.name === name);
  assert.equal(byName('husklet_image_inspect').inputSchema.safeParse({ reference: 'a'.repeat(257) }).success, false);
  assert.equal(byName('husklet_image_remove').inputSchema.safeParse({ reference: 'alpine:3.20' }).success, false);
  assert.equal(byName('husklet_image_remove').inputSchema.safeParse({ reference: 'sha256:abc', confirm: true }).success, false);
  assert.equal(byName('husklet_image_prune').inputSchema.safeParse({ confirm: false }).success, false);
  await byName('husklet_image_list').run({});
  await byName('husklet_image_inspect').run({ reference: 'sha256:abc' });
  await byName('husklet_image_pull').run({ reference: 'alpine:3.20' });
  const digest = `sha256:${'a'.repeat(64)}`;
  await byName('husklet_image_remove').run({ reference: digest, confirm: true });
  await byName('husklet_image_prune').run({ confirm: true });
  assert.deepEqual(calls, [
    ['images.list'],
    ['images.inspect', 'sha256:abc'],
    ['images.pull', 'alpine:3.20'],
    ['images.remove', digest],
    ['images.prune'],
  ]);
});

test('image pull jobs have strict identities and cancellation is not mislabeled destructive', async () => {
  const { api, calls } = fake(); const listed = tools(api); const byName = (name) => listed.find((tool) => tool.name === name);
  assert.equal(byName('husklet_image_pull_start').inputSchema.safeParse({ reference: 'alpine latest' }).success, false);
  assert.equal(byName('husklet_image_pull_status').inputSchema.safeParse({ job: '0' }).success, false);
  assert.equal(byName('husklet_image_pull_cancel').inputSchema.safeParse({ job: '7', confirm: true }).success, false);
  const value = async (name, input) => JSON.parse((await byName(name).run(input)).content[0].text);
  assert.deepEqual(await value('husklet_image_pull_start', { reference: 'alpine:3.20' }), { job: '7' });
  assert.deepEqual(await value('husklet_image_pull_status', { job: '7' }), { job: '7', revision: 1, state: 'starting' });
  assert.deepEqual(await value('husklet_image_pull_cancel', { job: '7' }), { done: true, job: '7' });
  assert.deepEqual(calls.filter(([name]) => name.startsWith('images.')), [
    ['images.startPull', 'alpine:3.20'], ['images.pullStatus', '7'], ['images.cancelPull', '7'],
  ]);
});

test('image pull wait filters exact job and revision and always disposes', async () => {
  const { api } = fake(); let listener; let disposed = 0;
  api.watchImagePulls = async (next) => { listener = next; return async () => { disposed += 1; }; };
  api.images.pullStatus = async (job) => ({ job, revision: 4, state: 'pulling', current: 5, total: 10 });
  const wait = tools(api).find(({ name }) => name === 'husklet_image_pull_wait');
  const pending = wait.run({ job: '7', after_revision: 2, timeout_ms: 1_000 }); await Promise.resolve();
  listener({ job: '8', revision: 9, state: 'complete', coalesced: 0 });
  listener({ job: '7', revision: 2, state: 'pulling', coalesced: 0 });
  listener({ job: '7', revision: 4, state: 'pulling', coalesced: 1 });
  const answer = JSON.parse((await pending).content[0].text);
  assert.equal(answer.changed, true); assert.equal(answer.change.job, '7'); assert.equal(answer.status.job, '7'); assert.equal(answer.status.revision, 4); assert.equal(disposed, 1);
  const timeout = JSON.parse((await wait.run({ job: '7', after_revision: 4, timeout_ms: 1 })).content[0].text);
  assert.deepEqual(timeout, { changed: false, job: '7', after_revision: 4 }); assert.equal(disposed, 2);
});

test('volume and network tools preserve typed read/control operations and confirmations', async () => {
  const { api, calls } = fake();
  const listed = tools(api);
  const byName = (name) => listed.find((tool) => tool.name === name);
  assert.equal(byName('husklet_volume_remove').inputSchema.safeParse({ name: 'cache' }).success, false);
  assert.equal(byName('husklet_volume_remove').inputSchema.safeParse({ name: 'cache', generation: 'short', confirm: true }).success, false);
  assert.equal(byName('husklet_network_remove').inputSchema.safeParse({ reference: 'private' }).success, false);
  assert.equal(byName('husklet_network_disconnect').inputSchema.safeParse({ reference: 'private', container: 'c1' }).success, false);
  assert.equal(byName('husklet_network_connect').inputSchema.safeParse({ reference: 'private', container: 'c1', extra: true }).success, false);
  assert.equal(byName('husklet_volume_create').inputSchema.safeParse({ name: 'a'.repeat(255) }).success, true);
  assert.equal(byName('husklet_volume_create').inputSchema.safeParse({ name: 'a'.repeat(256) }).success, false);
  assert.equal(byName('husklet_volume_create').inputSchema.safeParse({ name: '-leading' }).success, false);
  assert.equal(byName('husklet_network_create').inputSchema.safeParse({ name: 'é' }).success, false);
  const networkId = 'a'.repeat(32);
  const containerId = 'b'.repeat(64);
  assert.equal(byName('husklet_network_connect').inputSchema.safeParse({ reference: networkId, container: 'friendly' }).success, false);
  assert.equal(byName('husklet_network_connect').inputSchema.safeParse({ reference: networkId, container: containerId, aliases: Array(64).fill(0).map((_, index) => `alias-${index}`) }).success, true);
  assert.equal(byName('husklet_network_connect').inputSchema.safeParse({ reference: networkId, container: containerId, aliases: Array(65).fill('alias') }).success, false);
  assert.equal(byName('husklet_network_connect').inputSchema.safeParse({ reference: networkId, container: containerId, aliases: ['same', 'same'] }).success, false);
  await byName('husklet_volume_list').run({});
  await byName('husklet_volume_inspect').run({ name: 'cache' });
  await byName('husklet_volume_create').run({ name: 'build' });
  const volumeGeneration = 'c'.repeat(32);
  await byName('husklet_volume_remove').run({ name: 'old', generation: volumeGeneration, confirm: true });
  await byName('husklet_network_list').run({});
  await byName('husklet_network_inspect').run({ reference: 'private' });
  await byName('husklet_network_create').run({ name: 'backend' });
  await byName('husklet_network_remove').run({ reference: networkId, confirm: true });
  await byName('husklet_network_connect').run({ reference: networkId, container: containerId, aliases: ['database'] });
  await byName('husklet_network_disconnect').run({ reference: networkId, container: containerId, confirm: true });
  assert.deepEqual(calls, [
    ['volumes.list'], ['volumes.inspect', 'cache'], ['volumes.create', 'build'], ['volumes.remove', 'old', volumeGeneration],
    ['networks.list'], ['networks.inspect', 'private'], ['networks.create', 'backend'], ['networks.remove', networkId],
    ['networks.connect', networkId, containerId, { aliases: ['database'] }], ['networks.disconnect', networkId, containerId],
  ]);
});

test('unified pane XML packs terminal metadata and escaped bounded screen lines', async () => {
  const terminal = {
    panes: async () => ({ panes: [{ slot: 'term-1', kind: 'terminal' }], truncated: false }),
    topology: async () => ({ active_tab: 'tab-1', tabs: [{ id: 'tab-1', title: 'Shell & work', root: {
      kind: 'pane', focused: true, grid: { columns: 80, rows: 24 },
      pane: { slot: 'term-1', occupant: 'terminal', working_directory: '/work<&>', command: 'bash', provider: null },
    } }] }),
    read: async () => ({ slot: 'term-1', columns: 120, rows: 40, lines: ['one < two', 'token output remains screen data'], truncated: false }),
    semantics: async () => { throw new Error('not semantic'); },
  };
  const xml = await paneXml(terminal, 'term-1', 20);
  assert.match(xml, /^<husklet-pane slot="term-1" occupant="terminal" generation="0" revision="0"><terminal /);
  assert.match(xml, /active="true" focused="true" columns="120" rows="40"/);
  assert.match(xml, /title="Shell &amp; work"/);
  assert.match(xml, /<line index="0">one &lt; two<\/line>/);
  assert.match(xml, /token output remains screen data/);
  assert(new TextEncoder().encode(xml).byteLength <= 64 * 1024);
  assert.match(xml, /<\/terminal><\/husklet-pane>$/);
});

test('unified pane XML selects surface semantics and gives a clear topology absence error', async () => {
  const terminal = {
    panes: async () => ({ panes: [{ slot: 'surface-1', generation: 0, revision: 2, kind: 'surface' }], truncated: false }),
    topology: async () => ({ active_tab: null, tabs: [{ id: 't', title: 'UI', root: {
      kind: 'pane', focused: false, grid: null,
      pane: { slot: 'surface-1', occupant: 'surface', working_directory: null, command: null, provider: { extension: 'demo', provider: 'main' } },
    } }] }),
    semantics: async (slot) => {
      if (slot === 'missing') throw new Error('no semantic pane');
      return { slot, generation: 0, revision: 2, truncated: false, root: {
        id: 1, role: 'password_entry', label: 'API token', value: 'never leak', disabled: false, actions: [], children: [],
      } };
    },
  };
  const xml = await paneXml(terminal, 'surface-1');
  assert.match(xml, /^<husklet-pane slot="surface-1" occupant="surface" generation="0" revision="2"><pane /);
  assert(!xml.includes('never leak'));
  assert.match(xml, /\[redacted\]/);
  await assert.rejects(() => paneXml(terminal, 'missing'), /absent from pane inventory/);
});

test('unified pane XML projects arbitrary native slots and explicitly rejects unknown kinds', async () => {
  const terminal = {
    panes: async () => ({ panes: [{ slot: 'settings-native', generation: 0, revision: 4, kind: 'native' }], truncated: false }),
    semantics: async (slot) => ({ slot, generation: 0, revision: 4, truncated: false, root: {
      id: 1, role: 'status', label: 'Settings', value: 'Ready', disabled: false, actions: [], children: [],
    } }),
  };
  assert.match(await paneXml(terminal, 'settings-native'), /occupant="native".*Settings/s);
  terminal.panes = async () => ({ panes: [{ slot: 'shot', kind: 'screenshot' }], truncated: false });
  await assert.rejects(() => paneXml(terminal, 'shot'), /unsupported occupant "screenshot"/);
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
    slot, generation: 6, revision: 7, truncated: false,
    root: { id: 0, role: 'column', label: 'A & <B>', value: null, disabled: false, destructive: false, actions: [], children: [
      { id: 3, role: 'button', label: 'Run', value: null, disabled: false, destructive: false, actions: ['invoke'], children: [] },
    ] },
  }; };
  api.terminal.act = async (slot, action) => { calls.push(['terminal.act', slot, action]); };
  const listed = tools(api);
  const snapshot = listed.find(({ name }) => name === 'husklet_pane_snapshot');
  const action = listed.find(({ name }) => name === 'husklet_pane_action');
  const shown = await snapshot.run({ slot: 'pane-1' });
  assert.equal(shown.content[0].text, '<pane slot="pane-1" generation="6" revision="7" truncated="false"><node id="0" role="column" disabled="false" destructive="false" actions=""><label>A &amp; &lt;B&gt;</label><node id="3" role="button" disabled="false" destructive="false" actions="invoke"><label>Run</label></node></node></pane>');
  await action.run({ slot: 'pane-1', generation: 6, revision: 7, node: 3, action: 'invoke' });
  assert.deepEqual(calls, [
    ['terminal.semantics', 'pane-1'],
    ['terminal.semantics', 'pane-1'],
    ['terminal.act', 'pane-1', { generation: 6, revision: 7, node: 3, action: 'invoke' }],
  ]);
  assert.equal(action.inputSchema.safeParse({ slot: 'pane-1', revision: 7, node: 3, action: 'run' }).success, false);
});

test('pane actions reject stale, absent, disabled and unadvertised controls before dispatch', async () => {
  const { api, calls } = fake();
  api.terminal.semantics = async () => ({ slot: 'pane-1', generation: 4, revision: 12, truncated: false, root: {
    id: 0, role: 'column', label: null, value: null, disabled: false, destructive: false, actions: [], children: [
      { id: 4, role: 'button', label: 'Pending', value: null, disabled: true, destructive: false, actions: [], children: [] },
      { id: 5, role: 'entry', label: 'Name', value: '', disabled: false, destructive: false, actions: ['change'], children: [] },
    ],
  }});
  api.terminal.act = async (...args) => calls.push(['terminal.act', ...args]);
  const action = tools(api).find(({ name }) => name === 'husklet_pane_action');
  await assert.rejects(action.run({ slot: 'pane-1', generation: 3, revision: 12, node: 5, action: 'change' }), /stale pane generation/);
  await assert.rejects(action.run({ slot: 'pane-1', generation: 4, revision: 11, node: 5, action: 'change' }), /stale semantic revision/);
  await assert.rejects(action.run({ slot: 'pane-1', generation: 4, revision: 12, node: 99, action: 'invoke' }), /is absent/);
  await assert.rejects(action.run({ slot: 'pane-1', generation: 4, revision: 12, node: 4, action: 'invoke' }), /is disabled/);
  await assert.rejects(action.run({ slot: 'pane-1', generation: 4, revision: 12, node: 5, action: 'invoke' }), /does not advertise invoke/);
  assert(!calls.some(([name]) => name === 'terminal.act'));
});

test('destructive semantic actions require an explicit MCP confirmation', async () => {
  const { api, calls } = fake();
  api.terminal.semantics = async () => ({ slot: 'workspace', generation: 0, revision: 9, truncated: false, root: {
    id: 0, role: 'navigation', label: null, value: null, disabled: false, destructive: false, actions: [], children: [
      { id: 4, role: 'button', label: 'Confirm removal', value: null, disabled: false, destructive: true, actions: ['invoke'], children: [] },
    ],
  }});
  api.terminal.act = async (...args) => calls.push(['terminal.act', ...args]);
  const action = tools(api).find(({ name }) => name === 'husklet_pane_action');
  await assert.rejects(
    action.run({ slot: 'workspace', generation: 0, revision: 9, node: 4, action: 'invoke' }),
    /requires confirm: true/,
  );
  assert(!calls.some(([name]) => name === 'terminal.act'));
  await action.run({ slot: 'workspace', generation: 0, revision: 9, node: 4, action: 'invoke', confirm: true });
  assert.deepEqual(calls.at(-1), ['terminal.act', 'workspace', { generation: 0, revision: 9, node: 4, action: 'invoke' }]);
});

test('pane wait returns only bounded invalidation metadata and releases its subscription', async () => {
  const { api } = fake();
  let listener;
  let disposed = 0;
  api.watchPaneChanges = async (next) => { listener = next; return async () => { disposed += 1; }; };
  const wait = tools(api).find(({ name }) => name === 'husklet_pane_wait');
  assert.equal(wait.inputSchema.safeParse({ slot: 'pane-2', after_generation: 2, timeout_ms: 10 }).success, false);
  assert.equal(wait.inputSchema.safeParse({ after_generation: 2, after_revision: 8, timeout_ms: 10 }).success, false);
  const pending = wait.run({ slot: 'pane-2', after_generation: 2, after_revision: 8, timeout_ms: 1000 });
  await new Promise((resolve) => setImmediate(resolve));
  listener({ slot: 'pane-1', kind: 'terminal', revision: 0, generation: 1, coalesced: 0 });
  listener({ slot: 'pane-2', kind: 'native', revision: 8, generation: 2, coalesced: 6 });
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(disposed, 0, 'the initial scan at the observed cursor must not settle the wait');
  listener({ slot: 'pane-2', kind: 'native', revision: 0, generation: 3, coalesced: 0 });
  const answer = await pending;
  assert.deepEqual(JSON.parse(answer.content[0].text), {
    changed: true,
    change: { slot: 'pane-2', kind: 'native', revision: 0, generation: 3, coalesced: 0 },
  });
  assert.equal(disposed, 1);
  assert(!answer.content[0].text.includes('lines'));
  assert(!answer.content[0].text.includes('value'));
});

test('concurrent pane waits advance independently from exact generation and revision cursors', async () => {
  const { api } = fake(); const listeners = new Set(); let disposed = 0;
  api.watchPaneChanges = async (next) => {
    listeners.add(next);
    return async () => { if (listeners.delete(next)) disposed += 1; };
  };
  const wait = tools(api).find(({ name }) => name === 'husklet_pane_wait');
  const first = wait.run({ slot: 'pane-2', after_generation: 2, after_revision: 7, timeout_ms: 1000 });
  const second = wait.run({ slot: 'pane-2', after_generation: 2, after_revision: 8, timeout_ms: 1000 });
  await new Promise((resolve) => setImmediate(resolve));
  for (const listener of [...listeners]) listener({ slot: 'pane-2', kind: 'native', generation: 2, revision: 8, coalesced: 0 });
  assert.equal(JSON.parse((await first).content[0].text).change.revision, 8);
  assert.equal(disposed, 1);
  for (const listener of [...listeners]) listener({ slot: 'pane-2', kind: 'native', generation: 2, revision: 9, coalesced: 0 });
  assert.equal(JSON.parse((await second).content[0].text).change.revision, 9);
  assert.equal(disposed, 2);
  assert.equal(listeners.size, 0);
});

test('workspace event wait filters one bounded batch and always disposes', async () => {
  const { api } = fake(); let listener; let disposed = 0;
  api.watchWorkspaceEvents = async (next) => { listener = next; return async () => { disposed += 1; }; };
  const wait = tools(api).find(({ name }) => name === 'husklet_workspace_event_wait');
  assert.equal(wait.inputSchema.safeParse({ slot: 'pane-1', timeout_ms: 1 }).success, false);
  assert.equal(wait.inputSchema.safeParse({ kind: 'key', slot: 'pane-1', timeout_ms: 1 }).success, true);
  assert.equal(wait.inputSchema.safeParse({ kind: 'focus', phase: 'press', timeout_ms: 1 }).success, false);
  const pending = wait.run({ kind: 'pointer', slot: 'pane-2', phase: 'press', timeout_ms: 1000 });
  await new Promise((resolve) => setImmediate(resolve));
  listener({ events: [{ event: 'pointer', phase: 'move', slot: 'pane-2', generation: 2, x: 1, y: 2, button: null, modifiers: [], delta_x: null, delta_y: null }], dropped: 2 });
  listener({ events: [{ event: 'pointer', phase: 'press', slot: 'pane-1', generation: 2, x: 1, y: 2, button: 1, modifiers: [], delta_x: null, delta_y: null }], dropped: 3 });
  listener({ events: [{ event: 'pointer', phase: 'press', slot: 'pane-2', generation: 7, x: 4, y: 5, button: 1, modifiers: ['shift'], delta_x: null, delta_y: null }], dropped: 4 });
  const answer = JSON.parse((await pending).content[0].text);
  assert.equal(answer.observed, true); assert.equal(answer.event.slot, 'pane-2'); assert.equal(answer.event.generation, 7); assert.equal(answer.dropped, 9);
  assert.equal(disposed, 1);
});

test('workspace composite mutation arms before authority, ignores unrelated changes, and disposes', async () => {
  const { api } = fake(); const order = []; let listener; let disposed = 0;
  api.watchWorkspaceLifecycle = async (next) => { order.push('subscribe'); listener = next; return async () => { order.push('unsubscribe'); disposed += 1; }; };
  api.start = async (name) => {
    order.push(`start:${name}`);
    listener({ workspace: 'other', action: 'start', revision: 1, coalesced: 0 });
    listener({ workspace: name, action: 'start', revision: 2, coalesced: 0 });
  };
  const mutate = tools(api).find(({ name }) => name === 'husklet_workspace_mutate_wait');
  assert.equal(mutate.inputSchema.safeParse({ operation: 'delete', name: 'managed', generation }).success, false);
  const answer = JSON.parse((await mutate.run({ operation: 'start', name: 'managed', timeout_ms: 1000 })).content[0].text);
  assert.deepEqual(answer, { result: { done: true }, change: { workspace: 'managed', action: 'start', revision: 2, coalesced: 0 } });
  assert.deepEqual(order, ['subscribe', 'start:managed', 'unsubscribe']);
  assert.equal(disposed, 1);
});

test('workspace composite mutation disposes on authority failure and observation timeout', async () => {
  const { api } = fake(); let disposed = 0;
  api.watchWorkspaceLifecycle = async () => async () => { disposed += 1; };
  api.stop = async () => { throw new Error('stop refused'); };
  const mutate = tools(api).find(({ name }) => name === 'husklet_workspace_mutate_wait');
  await assert.rejects(() => mutate.run({ operation: 'stop', name: 'managed', timeout_ms: 1000 }), /stop refused/);
  assert.equal(disposed, 1);
  api.stop = async () => {};
  await assert.rejects(() => mutate.run({ operation: 'stop', name: 'managed', timeout_ms: 1 }), /timed out waiting for stop/);
  assert.equal(disposed, 2);
});

test('execution change wait ignores its unchanged initial cursor and returns subscription credit', async () => {
  const { api } = fake();
  let listener; let disposed = 0;
  api.watchExecutions = async (next) => { listener = next; return async () => { disposed += 1; }; };
  const wait = tools(api).find(({ name }) => name === 'husklet_execution_change_wait');
  const after = { container_id: 'a'.repeat(64), running: true, exit_code: 0, pid: 17, command: ['/bin/job'], user: 'app' };
  assert.equal(wait.inputSchema.safeParse({ id: 'e2', after: { ...after, pid: 1.5 } }).success, false);
  assert.equal(wait.inputSchema.safeParse({ id: 'e2', absent: true }).success, false);
  assert.equal(wait.inputSchema.safeParse({ id: 'e2', after, absent: true, running: false }).success, false);
  const pending = wait.run({ id: 'e2', after, running: false, timeout_ms: 1000 });
  await new Promise((resolve) => setImmediate(resolve));
  listener({ executions: [{ id: 'e1', ...after }, { id: 'e2', ...after }], truncated: false });
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(disposed, 0, 'the unchanged initial catalogue must not settle the wait');
  listener({ executions: [{ id: 'e2', ...after, running: false, exit_code: 9, pid: 0 }], truncated: true });
  assert.deepEqual(JSON.parse((await pending).content[0].text), {
    changed: true, replaced: false, execution: { id: 'e2', ...after, running: false, exit_code: 9, pid: 0 }, truncated: true,
  });
  assert.equal(disposed, 1);
});

test('execution waits detect impossible same-id replacement and dispose concurrent cursors independently', async () => {
  const { api } = fake(); const listeners = new Set(); let disposed = 0;
  api.watchExecutions = async (next) => { listeners.add(next); return async () => { if (listeners.delete(next)) disposed += 1; }; };
  const wait = tools(api).find(({ name }) => name === 'husklet_execution_change_wait');
  const after = { container_id: 'a'.repeat(64), running: true, exit_code: 0, pid: 17, command: ['/bin/job'], user: 'app' };
  const replacement = wait.run({ id: 'e2', after, running: false, timeout_ms: 1000 });
  const transition = wait.run({ id: 'e2', after: { ...after, container_id: 'b'.repeat(64) }, running: false, timeout_ms: 1000 });
  await new Promise((resolve) => setImmediate(resolve));
  for (const listener of [...listeners]) listener({ executions: [{ id: 'e2', ...after, container_id: 'b'.repeat(64) }], truncated: false });
  assert.equal(JSON.parse((await replacement).content[0].text).replaced, true);
  assert.equal(disposed, 1);
  for (const listener of [...listeners]) listener({ executions: [{ id: 'e2', ...after, container_id: 'b'.repeat(64), running: false, pid: 0 }], truncated: false });
  assert.equal(JSON.parse((await transition).content[0].text).replaced, false);
  assert.equal(disposed, 2);
});

test('execution removal wait ignores its present initial record and settles only on later absence', async () => {
  const { api } = fake(); let listener; let disposed = 0;
  api.watchExecutions = async (next) => { listener = next; return async () => { disposed += 1; }; };
  const wait = tools(api).find(({ name }) => name === 'husklet_execution_change_wait');
  const after = { container_id: 'a'.repeat(64), running: false, exit_code: 0, pid: 0, command: ['/bin/job'], user: 'app' };
  const pending = wait.run({ id: 'removed', after, absent: true, timeout_ms: 1000 });
  await new Promise((resolve) => setImmediate(resolve));
  listener({ executions: [{ id: 'removed', ...after }], truncated: false });
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(disposed, 0, 'the observed record in the initial catalogue must not settle removal');
  listener({ executions: [], truncated: true });
  assert.deepEqual(JSON.parse((await pending).content[0].text), {
    changed: true, execution: null, removed: true, truncated: true,
  });
  assert.equal(disposed, 1);
});

test('container change wait rejects its unchanged initial cursor and disposes after a transition', async () => {
  const { api } = fake(); let listener; let disposed = 0;
  api.watchContainers = async (next) => { listener = next; return async () => { disposed += 1; }; };
  const wait = tools(api).find(({ name }) => name === 'husklet_container_change_wait');
  assert.equal(wait.inputSchema.safeParse({ id: 'c1', state: 'running', absent: true }).success, false);
  assert.equal(wait.inputSchema.safeParse({ id: 'c2', after: { state: 'running' } }).success, false);
  const pending = wait.run({ id: 'c2', after: { state: 'running', created: 41 }, timeout_ms: 1000 });
  await new Promise((resolve) => setImmediate(resolve));
  listener([{ id: 'c1', state: 'exited' }, { id: 'c2', state: 'running', created: 41 }]);
  await new Promise((resolve) => setImmediate(resolve));
  assert.equal(disposed, 0, 'the subscription initial snapshot must not settle at the observed cursor');
  listener([{ id: 'c2', state: 'exited', created: 41, name: 'worker' }]);
  assert.deepEqual(JSON.parse((await pending).content[0].text), { changed: true, container: { id: 'c2', state: 'exited', created: 41, name: 'worker' } });
  assert.equal(disposed, 1);
});

test('container change wait treats a changed creation identity as replacement and isolates concurrent cursors', async () => {
  const { api } = fake(); const listeners = new Set(); let disposed = 0;
  api.watchContainers = async (next) => {
    listeners.add(next);
    return async () => { if (listeners.delete(next)) disposed += 1; };
  };
  const wait = tools(api).find(({ name }) => name === 'husklet_container_change_wait');
  const replaced = wait.run({ id: 'c2', after: { state: 'running', created: 41 }, timeout_ms: 1000 });
  const stopped = wait.run({ id: 'c2', after: { state: 'running', created: 42 }, state: 'exited', timeout_ms: 1000 });
  await new Promise((resolve) => setImmediate(resolve));
  for (const listener of [...listeners]) listener([{ id: 'c2', state: 'running', created: 42 }]);
  assert.equal(JSON.parse((await replaced).content[0].text).container.created, 42);
  assert.equal(disposed, 1);
  for (const listener of [...listeners]) listener([{ id: 'c2', state: 'exited', created: 42 }]);
  assert.equal(JSON.parse((await stopped).content[0].text).container.state, 'exited');
  assert.equal(disposed, 2);
  assert.equal(listeners.size, 0);
});

test('semantic XML escapes every XML metacharacter and remains structurally bounded', () => {
  assert.throws(() => semanticXml({ slot: 'legacy', revision: 1 }), /requires nonnegative safe integer generation/);
  const hostile = `&<>"'`;
  assert.equal(semanticXml({ slot: hostile, generation: 2, revision: 3, truncated: false, root: {
    id: hostile, role: hostile, label: hostile, value: '[redacted]', disabled: true, destructive: false,
    actions: [hostile], children: [],
  }}), '<pane slot="&amp;&lt;&gt;&quot;&apos;" generation="2" revision="3" truncated="false"><node id="&amp;&lt;&gt;&quot;&apos;" role="&amp;&lt;&gt;&quot;&apos;" disabled="true" destructive="false" actions="&amp;&lt;&gt;&quot;&apos;"><label>&amp;&lt;&gt;&quot;&apos;</label><value>[redacted]</value></node></pane>');
  const secret = semanticXml({ slot: 's', generation: 1, revision: 1, truncated: false, root: {
    id: 1, role: 'password_entry', label: 'Password', value: 'must-not-leak', disabled: false, actions: [], children: [],
  }});
  assert(!secret.includes('must-not-leak'));
  assert.match(secret, /<value>\[redacted\]<\/value>/);
  const controls = semanticXml({ slot: '\u0000\uD800', generation: 1, revision: 1, truncated: false, root: {
    id: 1, role: 'text', label: '\u0001', value: null, disabled: false, actions: [], children: [],
  }});
  assert(!/[\u0000-\u0008\uD800-\uDFFF]/.test(controls));
  assert.match(controls, /�/);

  const children = Array.from({ length: 400 }, (_, id) => ({
    id, role: 'text', label: 'x'.repeat(1000), value: null, disabled: false, actions: [], children: [],
  }));
  const bounded = semanticXml({ slot: 'large', generation: 1, revision: 4, truncated: true, root: {
    id: 0, role: 'column', label: null, value: null, disabled: false, actions: [], children,
  }});
  assert(new TextEncoder().encode(bounded).byteLength <= 64 * 1024);
  assert.match(bounded, /<truncated\/>/);
  assert.match(bounded, /^<pane .*<\/pane>$/);
  assert.equal((bounded.match(/<node /g) ?? []).length, (bounded.match(/<\/node>/g) ?? []).length);
  const plan=semanticXml({slot:'plan',generation:1,revision:1,truncated:false,root:{id:0,role:'QueryPlan',label:'plan & <x>',value:'bounded source',disabled:false,destructive:false,actions:[],children:[{id:1,role:'QueryPlanNode',label:'hash_join',value:'state=hot detail="slow&wide"',disabled:false,destructive:false,actions:[],children:[{id:2,role:'QueryPlanMetric',label:'duration_us',value:'42',disabled:false,destructive:false,actions:[],children:[]}]}]}});assert.match(plan,/role="QueryPlan"/);assert.match(plan,/plan &amp; &lt;x&gt;/);assert.match(plan,/detail=&quot;slow&amp;wide&quot;/);assert.equal((plan.match(/<node /g)??[]).length,3);assert(!plan.includes('<truncated/>'));
  const dependency = semanticXml({slot:'deps',generation:1,revision:1,truncated:false,root:{id:0,role:'DependencyGraph',label:'deps & <graph>',value:'bounded source',disabled:false,destructive:false,actions:[],children:[{id:1,role:'DependencyNode',label:'react',value:'state=conflict detail="18&19"',disabled:false,destructive:false,actions:[],children:[{id:2,role:'DependencyEdge',label:'runtime → scheduler',value:'requirement=<19',disabled:false,destructive:false,actions:[],children:[]}]}]}}); assert.match(dependency,/role="DependencyGraph"/);assert.match(dependency,/&amp;/);assert.match(dependency,/&lt;19/);assert.equal((dependency.match(/<node /g)??[]).length,3);
  const waterfall = semanticXml({ slot: 'net', generation: 1, revision: 5, truncated: false, root: { id: 0, role: 'NetworkWaterfall', label: 'requests', value: 'showing all 1 requests', disabled: false, destructive: false, actions: [], children: [{ id: 1, role: 'NetworkRequest', label: 'GET https://example.test?a=&b=<', value: 'status=200 detail="ok"', disabled: false, destructive: false, actions: [], children: [{ id: 2, role: 'NetworkPhase', label: 'wait', value: 'offset_us=2 duration_us=3 total_us=10', disabled: false, destructive: false, actions: [], children: [] }] }] } });
  assert.match(waterfall, /role="NetworkWaterfall"/); assert.match(waterfall, /a=&amp;b=&lt;/); assert.match(waterfall, /detail=&quot;ok&quot;/); assert.equal((waterfall.match(/<node /g) ?? []).length, 3);
});

test('semantic and terminal XML identify every field-local clipping boundary', async () => {
  const long = '🧪'.repeat(257);
  const xml = semanticXml({ slot: 's', generation: 1, revision: 1, truncated: false, root: {
    id: long, role: long, label: long, value: long, disabled: false, destructive: false,
    actions: Array.from({ length: 17 }, (_, index) => `action-${index}`), children: [],
  } });
  assert.match(xml, /id-truncated="true"/); assert.match(xml, /role-truncated="true"/);
  assert.match(xml, /actions-truncated="true"/); assert.match(xml, /<label truncated="true">/);
  assert.match(xml, /<value truncated="true">/); assert(!xml.includes('\uFFFD'));
  const terminal = { panes: async () => ({ panes: [{ slot: 't', kind: 'terminal', generation: 1, revision: 1 }] }),
    topology: async () => ({ active_tab: 'tab', tabs: [{ id: 'tab', title: 'T', root: { kind: 'pane', pane: { slot: 't' }, grid: null, focused: false } }] }),
    read: async () => ({ generation: 1, revision: 1, lines: ['🧪'.repeat(4097)], truncated: false }) };
  const pane = await paneXml(terminal, 't');
  assert.match(pane, /<line index="0" truncated="true">/); assert(!pane.includes('\uFFFD'));
});

test('a real MCP client lists strict tools and calls through the React session contract', async () => {
  const calls = [];
  const events = new Set();
  const session = {
    onEvent: (listener) => { events.add(listener); return () => events.delete(listener); },
    call: async (name, argument) => {
      calls.push([name, argument]);
      if (name === 'workspace_info') return { reply: 'workspace', with: { name: 'demo' } };
      if (name === 'extension_list') return { reply: 'extensions', with: [{ name: 'manager', image_digest: `sha256:${'a'.repeat(64)}`, status: 'standby' }] };
      if (name === 'extension_disable') return { reply: 'done' };
      if (name === 'extension_acquisition_start') return { reply: 'extension_acquisition_job', with: { job: 'job-live' } };
      if (name === 'extension_acquisition_status') return { reply: 'extension_acquisition', with: { job: 'job-live', reference: 'example:1', revision: 3, state: 'ready', candidate: { name: 'example', version: '1', image_digest: 'sha256:def', requested: ['interface'], installed_image_digest: 'sha256:abc' }, error: null } };
      if (name === 'extension_install') return { reply: 'extension', with: { name: 'example', image_digest: 'sha256:def', status: 'standby' } };
      if (name === 'execution_inspect') return { reply: 'execution', with: {
        id: argument.id, container_id: 'container-1', running: true, exit_code: null,
      } };
      if (name === 'execution_kill') return { reply: 'done' };
      if (name === 'container_stop' || name === 'container_kill') return { reply: 'done' };
      if (name === 'image_list') return { reply: 'images', with: [{ id: 'sha256:abc', references: ['alpine:3.20'], size: 123 }] };
      if (name === 'volume_list') return { reply: 'volumes', with: [{ name: 'cache', driver: 'local' }] };
      if (name === 'network_list') return { reply: 'networks', with: [{ id: 'n1', name: 'private', driver: 'bridge' }] };
      if (name === 'pane_semantic_read') return { reply: 'semantics', with: {
        slot: argument.slot, generation: 13, revision: 11, truncated: false,
        root: { id: 0, role: 'column', label: 'Live', value: null, disabled: false, actions: ['invoke'], children: [] },
      } };
      if (name === 'pane_semantic_action') return { reply: 'done' };
      if (name === 'terminal_spawn_observed') return { reply: 'done' };
      if (name === 'terminal_write_pane') return { reply: 'done' };
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
  assert(listed.tools.some(({ name }) => name === 'husklet_extension_list'));
  assert(listed.tools.some(({ name }) => name === 'husklet_container_execution'));
  assert(listed.tools.some(({ name }) => name === 'husklet_execution_wait'));
  assert(listed.tools.some(({ name }) => name === 'husklet_execution_signal'));
  assert(listed.tools.some(({ name }) => name === 'husklet_image_list'));
  assert(listed.tools.some(({ name }) => name === 'husklet_volume_list'));
  assert(listed.tools.some(({ name }) => name === 'husklet_network_list'));
  assert(listed.tools.some(({ name }) => name === 'husklet_pane_snapshot'));
  assert(listed.tools.some(({ name }) => name === 'husklet_pane_read'));
  assert(listed.tools.some(({ name }) => name === 'husklet_pane_action'));
  assert(listed.tools.some(({ name }) => name === 'husklet_pane_wait'));
  assert(listed.tools.some(({ name }) => name === 'husklet_terminal_write_bytes'));
  assert(listed.tools.some(({ name }) => name === 'husklet_terminal_spawn'));
  const answer = await client.callTool({ name: 'husklet_workspace_info', arguments: {} });
  assert.equal(answer.content[0].text, '{"name":"demo"}');
  const extensions = await client.callTool({ name: 'husklet_extension_list', arguments: {} });
  assert.deepEqual(JSON.parse(extensions.content[0].text), [{ name: 'manager', image_digest: `sha256:${'a'.repeat(64)}`, status: 'standby' }]);
  await client.callTool({ name: 'husklet_extension_disable', arguments: { name: 'manager', image_digest: `sha256:${'a'.repeat(64)}`, confirm: true } });
  const acquired = await client.callTool({ name: 'husklet_extension_acquire', arguments: { reference: 'example:1', confirm: true } });
  assert.equal(JSON.parse(acquired.content[0].text).job, 'job-live');
  const candidate = await client.callTool({ name: 'husklet_extension_acquisition', arguments: { job: 'job-live' } });
  assert.equal(JSON.parse(candidate.content[0].text).candidate.image_digest, 'sha256:def');
  await client.callTool({ name: 'husklet_extension_install', arguments: { job: 'job-live', revision: 3, granted: ['interface'], confirm: true } });
  const immutableExecution = 'b'.repeat(32);
  const execution = await client.callTool({ name: 'husklet_container_execution', arguments: { id: immutableExecution } });
  assert.deepEqual(JSON.parse(execution.content[0].text), {
    id: immutableExecution, container_id: 'container-1', running: true, exit_code: null,
  });
  await client.callTool({ name: 'husklet_execution_wait', arguments: { id: immutableExecution, timeout_ms: 250 } });
  await client.callTool({ name: 'husklet_execution_signal', arguments: { id: 'b'.repeat(32), signal: 'SIGHUP', confirm: true } });
  const refusedStop = await client.callTool({ name: 'husklet_container_stop', arguments: { id: 'container-1' } });
  assert.equal(refusedStop.isError, true);
  const refusedKill = await client.callTool({ name: 'husklet_container_kill', arguments: { id: 'container-1', signal: 'SIGKILL' } });
  assert.equal(refusedKill.isError, true);
  await client.callTool({ name: 'husklet_container_stop', arguments: { id: 'a'.repeat(64), confirm: true } });
  await client.callTool({ name: 'husklet_container_kill', arguments: { id: 'a'.repeat(64), signal: 'SIGKILL', confirm: true } });
  const images = await client.callTool({ name: 'husklet_image_list', arguments: {} });
  assert.deepEqual(JSON.parse(images.content[0].text), [{ id: 'sha256:abc', references: ['alpine:3.20'], size: 123 }]);
  const volumes = await client.callTool({ name: 'husklet_volume_list', arguments: {} });
  assert.deepEqual(JSON.parse(volumes.content[0].text), [{ name: 'cache', driver: 'local' }]);
  const networks = await client.callTool({ name: 'husklet_network_list', arguments: {} });
  assert.deepEqual(JSON.parse(networks.content[0].text), [{ id: 'n1', name: 'private', driver: 'bridge' }]);
  await client.callTool({ name: 'husklet_terminal_spawn', arguments: {
    slot: 'pane-live', generation: 13, revision: 11, command: ['printf', '%s\n', 'ready'],
  } });
  await client.callTool({ name: 'husklet_terminal_write_bytes', arguments: {
    slot: 'pane-live', generation: 13, revision: 11, input_base64: Buffer.from([0, 3, 0x80, 0xff]).toString('base64'),
  } });
  const snapshot = await client.callTool({ name: 'husklet_pane_snapshot', arguments: { slot: 'pane-live' } });
  assert.match(snapshot.content[0].text, /^<pane slot="pane-live" generation="13" revision="11"/);
  await client.callTool({ name: 'husklet_pane_action', arguments: { slot: 'pane-live', generation: 13, revision: 11, node: 0, action: 'invoke' } });
  const waited = await client.callTool({ name: 'husklet_pane_wait', arguments: { slot: 'pane-live', timeout_ms: 1000 } });
  assert.deepEqual(JSON.parse(waited.content[0].text).change, {
    slot: 'pane-live', kind: 'surface', revision: 12, generation: 13, coalesced: 2,
  });
  assert.deepEqual(calls, [
    ['workspace_info', undefined],
    ['extension_list', undefined],
    ['extension_disable', { name: 'manager', image_digest: `sha256:${'a'.repeat(64)}` }],
    ['extension_acquisition_start', { reference: 'example:1' }],
    ['extension_acquisition_status', { job: 'job-live' }],
    ['extension_install', { job: 'job-live', revision: 3, granted: ['interface'] }],
    ['execution_inspect', { id: immutableExecution }],
    ['execution_wait', { id: immutableExecution, timeout_ms: 250 }],
    ['execution_kill', { id: 'b'.repeat(32), signal: 'SIGHUP' }],
    ['container_stop', { id: 'a'.repeat(64) }],
    ['container_kill', { id: 'a'.repeat(64), signal: 'SIGKILL' }],
    ['image_list', undefined],
    ['volume_list', undefined],
    ['network_list', undefined],
    ['terminal_spawn_observed', { slot: 'pane-live', generation: 13, revision: 11, command: ['printf', '%s\n', 'ready'] }],
    ['terminal_write_pane', { slot: 'pane-live', generation: 13, revision: 11, contents: [0, 3, 128, 255] }],
    ['pane_semantic_read', { slot: 'pane-live' }],
    ['pane_semantic_read', { slot: 'pane-live' }],
    ['pane_semantic_action', { slot: 'pane-live', action: { generation: 13, revision: 11, node: 0, action: 'invoke' } }],
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
    if (name === 'pane_list') return { reply: 'panes', with: { panes: [
      { slot: 'term', generation: 4, revision: 7, kind: 'terminal', provider: null, tab: 'tab', title: 'Packed', focused: true },
      { slot: 'surface', generation: 5, revision: 9, kind: 'surface', provider: null, tab: 'tab', title: 'Packed', focused: false },
    ], truncated: false } };
    if (name === 'terminal_topology') return { reply: 'topology', with: { active_tab: 'tab', tabs: [{ id: 'tab', title: 'Packed', root: {
      kind: 'split', division: 'beside', ratio_per_mille: 500, first: pane('term', 'terminal'), second: pane('surface', 'surface'),
    } }] } };
    if (name === 'terminal_read_pane') return { reply: 'text', with: { slot: argument.slot, generation: 4, revision: 7, lines: ['hello & goodbye'], truncated: false } };
    if (name === 'pane_semantic_read') return { reply: 'semantics', with: { slot: argument.slot, generation: 5, revision: 9, truncated: false,
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
    'pane_list', 'terminal_topology', 'terminal_read_pane', 'pane_list', 'pane_semantic_read',
  ]);
  await client.close();
  await server.close();
});

test('pane XML refuses text and semantics from a replaced occupant', async () => {
  const descriptor = (kind) => ({ slot: 'pane', generation: 8, revision: 13, kind, provider: null });
  const terminal = {
    panes: async () => ({ panes: [descriptor('terminal')], truncated: false }),
    topology: async () => ({ active_tab: 'tab', tabs: [{ id: 'tab', title: 'Tab', root: {
      kind: 'pane', focused: true, grid: { columns: 80, rows: 24 },
      pane: { slot: 'pane', occupant: 'terminal', working_directory: null, command: null, provider: null },
    } }] }),
    read: async () => ({ slot: 'pane', generation: 9, revision: 13, lines: ['wrong occupant'], truncated: false }),
  };
  await assert.rejects(paneXml(terminal, 'pane'), /changed while it was being read/);

  terminal.panes = async () => ({ panes: [descriptor('surface')], truncated: false });
  terminal.semantics = async () => ({ slot: 'pane', generation: 8, revision: 14, truncated: false,
    root: { id: 1, role: 'label', label: 'stale', value: null, disabled: false, actions: [], children: [] } });
  await assert.rejects(paneXml(terminal, 'pane'), /changed while it was being read/);
});

test('pane XML follows every split leaf and refuses a removed stale slot', async () => {
  let changed = false;
  const calls = [];
  const leaf = (slot, focused, columns, rows) => ({
    kind: 'pane', focused, grid: { columns, rows },
    pane: { slot, occupant: 'terminal', working_directory: `/work/${slot}`, command: `shell-${slot}`, provider: null },
  });
  const leafGrid = (slot) => {
    if (slot === 'left') return [72, 30];
    if (slot === 'upper') return [48, 12];
    if (slot === 'right') return changed ? [132, 41] : [48, 18];
    return [90, 25];
  };
  const session = { call: async (name, argument) => {
    calls.push([name, argument]);
    if (name === 'pane_list') {
      const slots = changed ? ['right'] : ['left', 'upper', 'right', 'other-tab'];
      return { reply: 'panes', with: { panes: slots.map((slot) => ({ slot, kind: 'terminal', provider: null, tab: null, title: null, focused: false })), truncated: false } };
    }
    if (name === 'terminal_topology') return { reply: 'topology', with: {
      active_tab: changed ? 'tab-b' : 'tab-a',
      tabs: changed ? [{ id: 'tab-b', title: 'After', root: leaf('right', true, 132, 41) }] : [
        { id: 'tab-a', title: 'Before', root: { kind: 'split', division: 'beside', ratio_per_mille: 600,
          first: leaf('left', true, 72, 30), second: { kind: 'split', division: 'below', ratio_per_mille: 400,
            first: leaf('upper', false, 48, 12), second: leaf('right', false, 48, 18) } } },
        { id: 'tab-b', title: 'Background', root: leaf('other-tab', true, 90, 25) },
      ],
    } };
    if (name === 'terminal_read_pane') return { reply: 'text', with: {
      slot: argument.slot, columns: leafGrid(argument.slot)[0], rows: leafGrid(argument.slot)[1],
      lines: [`visible <${argument.slot}>`], cursor_column: 6, cursor_row: 7,
      truncated: argument.slot === 'upper',
    } };
    // A stale host cache must never make a removed split leaf look native.
    if (name === 'pane_semantic_read') return { reply: 'semantics', with: {
      slot: argument.slot, revision: 99, truncated: false,
      root: { id: 1, role: 'status', label: 'stale', value: 'removed', disabled: false, destructive: false, actions: [], children: [] },
    } };
    throw new Error(`unexpected call ${name}`);
  } };
  const server = createServer(session);
  const client = new Client({ name: 'split-reader', version: '1' });
  const [clientTransport, serverTransport] = InMemoryTransport.createLinkedPair();
  await Promise.all([server.connect(serverTransport), client.connect(clientTransport)]);

  const expected = [
    ['left', 'tab-a', 'true', 'true', '72', '30', 'false'],
    ['upper', 'tab-a', 'true', 'false', '48', '12', 'true'],
    ['right', 'tab-a', 'true', 'false', '48', '18', 'false'],
    ['other-tab', 'tab-b', 'false', 'true', '90', '25', 'false'],
  ];
  for (const [slot, tab, active, focused, columns, rows, truncated] of expected) {
    const answer = await client.callTool({ name: 'husklet_pane_read', arguments: { slot, lines: 10 } });
    const xml = answer.content[0].text;
    assert.match(xml, new RegExp(`<husklet-pane slot="${slot}" occupant="terminal" generation="0" revision="0">`));
    assert.match(xml, new RegExp(`<terminal tab="${tab}"[^>]*active="${active}"[^>]*focused="${focused}"[^>]*columns="${columns}" rows="${rows}" cursor-column="6" cursor-row="7"[^>]*truncated="${truncated}">`));
    assert.match(xml, new RegExp(`visible &lt;${slot}&gt;`));
  }

  changed = true;
  const surviving = await client.callTool({ name: 'husklet_pane_read', arguments: { slot: 'right', lines: 10 } });
  assert.match(surviving.content[0].text, /tab="tab-b" title="After" active="true" focused="true" columns="132" rows="41"/);
  const removed = await client.callTool({ name: 'husklet_pane_read', arguments: { slot: 'upper', lines: 10 } });
  assert.equal(removed.isError, true);
  assert.match(removed.content[0].text, /absent from pane inventory/);
  assert.equal(calls.filter(([name]) => name === 'pane_semantic_read').length, 0, 'removed slots never probe stale semantics');

  await client.close();
  await server.close();
});

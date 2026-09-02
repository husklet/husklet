import assert from 'node:assert/strict';
import test from 'node:test';
import { Client } from '@modelcontextprotocol/sdk/client/index.js';
import { InMemoryTransport } from '@modelcontextprotocol/sdk/inMemory.js';
import { createServer, paneXml, semanticXml, tools } from '../src/index.js';

function fake() {
  const calls = [];
  const record = (name, answer = { ok: true }) => async (...args) => { calls.push([name, ...args]); return answer; };
  return { calls, api: {
    info: record('info', { name: 'demo', token: 'never expose me' }), list: record('list'), inspect: record('inspect'), create: record('workspace.create'), update: record('workspace.update'),
    start: record('workspace.start'), stop: record('workspace.stop'), restart: record('workspace.restart'), delete: record('workspace.delete'),
    extensions: { list: record('extensions.list'), inspect: record('extensions.inspect'), enable: record('extensions.enable'), disable: record('extensions.disable'), remove: record('extensions.remove'), startAcquisition: record('extensions.startAcquisition'), acquisition: record('extensions.acquisition'), cancelAcquisition: record('extensions.cancelAcquisition'), install: record('extensions.install'), update: record('extensions.update') },
    containers: { list: record('containers.list'), inspect: record('containers.inspect'), processes: record('containers.processes'), execution: record('containers.execution'), executions: record('containers.executions'), executionLogs: record('containers.executionLogs'), waitExecution: record('containers.waitExecution'), signalExecution: record('containers.signalExecution'), removeExecution: record('containers.removeExecution'), logs: record('containers.logs'), create: record('containers.create'), exec: record('containers.exec'), start: record('containers.start'), stop: record('containers.stop'), pause: record('containers.pause'), unpause: record('containers.unpause'), restart: record('containers.restart'), remove: record('containers.remove'), kill: record('containers.kill') },
    images: { list: record('images.list'), inspect: record('images.inspect'), pull: record('images.pull'), startPull: record('images.startPull', { job: '7' }), pullStatus: record('images.pullStatus', { job: '7', revision: 1, state: 'starting' }), cancelPull: record('images.cancelPull'), remove: record('images.remove'), prune: record('images.prune') },
    volumes: { list: record('volumes.list'), inspect: record('volumes.inspect'), create: record('volumes.create'), remove: record('volumes.remove') },
    networks: { list: record('networks.list'), inspect: record('networks.inspect'), create: record('networks.create'), remove: record('networks.remove'), connect: record('networks.connect'), disconnect: record('networks.disconnect') },
    terminal: { tabs: record('terminal.tabs'), topology: record('terminal.topology'), read: record('terminal.read'), writeInput: record('terminal.writeInput'), openTab: record('terminal.openTab'), split: record('terminal.split'), spawn: record('terminal.spawn'), focus: record('terminal.focus'), resizeGrid: record('terminal.resizeGrid'), ratio: record('terminal.ratio'), close: record('terminal.close') },
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
  await update.run({ name: 'dev', configuration: value, confirm: true });
  assert.deepEqual(calls, [['workspace.create', value], ['workspace.update', 'dev', value]]);
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
    name: 'husklet_workspace_update', arguments: { name: 'dev', configuration: value, confirm: true },
  });
  assert.equal(denied.isError, true);
  assert.match(denied.content[0].text, /workspace-control/);
  assert.deepEqual(calls, [
    ['workspace_create', { configuration: value }],
    ['workspace_update', { name: 'dev', configuration: value }],
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
  assert.equal(spawn.inputSchema.safeParse({ slot: 'pane-1', command: [] }).success, false);
  assert.equal(spawn.inputSchema.safeParse({ slot: 'pane-1', command: [''] }).success, false);
  assert.equal(spawn.inputSchema.safeParse({ slot: 'pane-1', command: ['x'.repeat(4097)] }).success, false);
  assert.equal(spawn.inputSchema.safeParse({ slot: 'pane-1', command: ['x'.repeat(513), ...Array(63).fill('x'.repeat(512))] }).success, false);
  await spawn.run({ slot: 'pane-1', command: ['printf', '%s\n', 'ready'] });
  const start = listed.find(({ name }) => name === 'husklet_container_start');
  assert.equal(start.inputSchema.safeParse({ id: 'abc', extra: true }).success, false);
  await start.run({ id: 'abc' });
  assert.deepEqual(calls, [
    ['terminal.spawn', 'pane-1', ['printf', '%s\n', 'ready']],
    ['containers.start', 'abc'],
  ]);
});

test('container termination requires confirmation before host authority is called', async () => {
  const { api, calls } = fake();
  const listed = tools(api);
  const stop = listed.find(({ name }) => name === 'husklet_container_stop');
  const kill = listed.find(({ name }) => name === 'husklet_container_kill');
  assert.equal(stop.inputSchema.safeParse({ id: 'abc' }).success, false);
  assert.equal(stop.inputSchema.safeParse({ id: 'abc', confirm: false }).success, false);
  assert.equal(kill.inputSchema.safeParse({ id: 'abc', signal: 'SIGKILL' }).success, false);
  assert.equal(kill.inputSchema.safeParse({ id: 'abc', signal: 'x'.repeat(33), confirm: true }).success, false);
  assert.deepEqual(calls, [], 'schema refusal cannot call host authority');
  await stop.run({ id: 'abc', confirm: true });
  await kill.run({ id: 'abc', signal: 'SIGKILL', confirm: true });
  assert.deepEqual(calls, [['containers.stop', 'abc'], ['containers.kill', 'abc', 'SIGKILL']]);
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
    await byName(`husklet_extension_${action}`).run({ name: 'workspace-manager', confirm: true });
  }
  await byName('husklet_extension_list').run({});
  await byName('husklet_extension_inspect').run({ name: 'workspace-manager' });
  assert.deepEqual(calls, [
    ['extensions.enable', 'workspace-manager'], ['extensions.disable', 'workspace-manager'],
    ['extensions.remove', 'workspace-manager'], ['extensions.list'], ['extensions.inspect', 'workspace-manager'],
  ]);
});

test('extension acquisition is asynchronous, digest-observable, grant-bounded, and confirmed', async () => {
  const { api, calls } = fake(); const listed = tools(api); const byName = (name) => listed.find((tool) => tool.name === name);
  for (const name of ['husklet_extension_acquire', 'husklet_extension_acquisition_cancel', 'husklet_extension_install', 'husklet_extension_update']) {
    assert.equal(byName(name).inputSchema.safeParse(name.endsWith('acquire') ? { reference: 'example:1' } : { job: 'j', revision: 1, granted: [], confirm: false }).success, false);
  }
  assert.equal(byName('husklet_extension_install').inputSchema.safeParse({ job: 'j', revision: 1, granted: ['made-up'], confirm: true }).success, false);
  assert.equal(byName('husklet_extension_install').inputSchema.safeParse({ job: 'j', revision: Number.MAX_SAFE_INTEGER + 1, granted: [], confirm: true }).success, false);
  await byName('husklet_extension_acquire').run({ reference: 'example:1', confirm: true });
  await byName('husklet_extension_acquisition').run({ job: 'j' });
  await byName('husklet_extension_acquisition_cancel').run({ job: 'j', confirm: true });
  await byName('husklet_extension_install').run({ job: 'j', revision: 4, granted: ['interface'], confirm: true });
  await byName('husklet_extension_update').run({ job: 'j', revision: 4, granted: ['interface'], confirm: true });
  assert.deepEqual(calls, [['extensions.startAcquisition', 'example:1'], ['extensions.acquisition', 'j'], ['extensions.cancelAcquisition', 'j'], ['extensions.install', 'j', 4, ['interface']], ['extensions.update', 'j', 4, ['interface']]]);
});

test('extension wait filters acquisition jobs and disposes its credit-controlled watcher', async () => {
  const { api } = fake(); let listener; let disposed = 0;
  api.watchExtensionAcquisitions = async (next) => { listener = next; return async () => { disposed += 1; }; };
  const wait = tools(api).find(({ name }) => name === 'husklet_extension_wait');
  assert.equal(wait.inputSchema.safeParse({ kind: 'inventory', job: 'j', timeout_ms: 10 }).success, false);
  const pending = wait.run({ kind: 'acquisition', job: 'wanted', timeout_ms: 1000 });
  await new Promise((resolve) => setImmediate(resolve));
  listener({ job: 'other', revision: 1, state: 'ready', coalesced: 0 });
  listener({ job: 'wanted', revision: 2, state: 'ready', coalesced: 3 });
  const answer = await pending;
  assert.deepEqual(JSON.parse(answer.content[0].text), { changed: true, change: { job: 'wanted', revision: 2, state: 'ready', coalesced: 3 } });
  assert.equal(disposed, 1);
});

test('container create and exec accept only bounded structured authority', async () => {
  const { api, calls } = fake();
  const listed = tools(api);
  const create = listed.find(({ name }) => name === 'husklet_container_create');
  const exec = listed.find(({ name }) => name === 'husklet_container_exec');
  assert.equal(create.inputSchema.safeParse({ image: 'alpine:3.20', name: 'worker-1' }).success, true);
  assert.equal(create.inputSchema.safeParse({ image: 'alpine latest', name: 'worker' }).success, false);
  assert.equal(create.inputSchema.safeParse({ image: 'alpine:3.20', name: '../worker' }).success, false);
  assert.equal(create.inputSchema.safeParse({ image: 'alpine:3.20', name: 'worker', mounts: [{ volume: 'cache', target: '../host', read_only: false }] }).success, false);
  assert.equal(create.inputSchema.safeParse({ image: 'alpine:3.20', name: 'worker', ports: [{ container: 80, host: 0, protocol: 'tcp' }] }).success, false);
  assert.equal(exec.inputSchema.safeParse({ id: 'c1', command: 'sh -lc whoami' }).success, false);
  assert.equal(exec.inputSchema.safeParse({ id: 'c1', command: [] }).success, false);
  assert.equal(exec.inputSchema.safeParse({ id: 'c1', command: Array(65).fill('x') }).success, false);
  assert.equal(exec.inputSchema.safeParse({ id: 'c1', command: ['true'], working_directory: 'relative' }).success, false);
  const spec = create.inputSchema.parse({
    image: 'alpine:3.20', name: 'worker-1', entrypoint: ['/usr/bin/env'], command: ['worker', '--once'],
    environment: [['MODE', 'agent']], working_directory: '/work', user: '1000', labels: [['owner', 'agent']],
    mounts: [{ volume: 'cache', target: '/cache', read_only: false }], network: 'private',
    ports: [{ container: 8080, host: 18080, protocol: 'tcp' }], memory_mb: 512, cpus: 2, pids_limit: 128,
  });
  await create.run(spec);
  await exec.run({ id: 'c1', command: ['printf', '%s', 'hello'], user: '1000', working_directory: '/work' });
  assert.deepEqual(calls, [
    ['containers.create', spec],
    ['containers.exec', 'c1', { command: ['printf', '%s', 'hello'], user: '1000', workingDirectory: '/work' }],
  ]);
});

test('filesystem controls are strict and removal requires explicit confirmation', async () => {
  const { api, calls } = fake();
  const listed = tools(api);
  const byName = (name) => listed.find((tool) => tool.name === name);
  assert.equal(byName('husklet_file_remove').inputSchema.safeParse({ path: 'old.txt' }).success, false);
  assert.equal(byName('husklet_file_rename').inputSchema.safeParse({ from: 'a', to: 'b', extra: true }).success, false);
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
  await execution.run({ id: 'exec-1' });
  assert.deepEqual(calls, [['containers.execution', 'exec-1']]);
});

test('execution wait is a strict bounded read and preserves the timeout', async () => {
  const { api, calls } = fake();
  const wait = tools(api).find(({ name }) => name === 'husklet_execution_wait');
  assert.equal(wait.inputSchema.safeParse({ id: 'e1', timeout_ms: 0 }).success, false);
  assert.equal(wait.inputSchema.safeParse({ id: 'e1', timeout_ms: 30_001 }).success, false);
  assert.equal(wait.inputSchema.safeParse({ id: 'e1', timeout_ms: 10, extra: true }).success, false);
  await wait.run({ id: 'e1', timeout_ms: 1250 });
  assert.deepEqual(calls, [['containers.waitExecution', 'e1', { timeoutMs: 1250 }]]);
});

test('execution catalogue and output are finite strict reads', async () => {
  const { api, calls } = fake();
  const listed = tools(api);
  const list = listed.find(({ name }) => name === 'husklet_execution_list');
  const logs = listed.find(({ name }) => name === 'husklet_execution_logs');
  assert.equal(logs.inputSchema.safeParse({ id: 'e1', stdout: false, stderr: false }).success, false);
  assert.equal(logs.inputSchema.safeParse({ id: 'e1', extra: true }).success, false);
  await list.run({});
  await logs.run({ id: 'e1', stdout: true, stderr: false });
  assert.deepEqual(calls, [['containers.executions'], ['containers.executionLogs', 'e1', { stdout: true, stderr: false }]]);
});

test('execution signaling targets an execution with a strict bounded signal', async () => {
  const { api, calls } = fake();
  const signal = tools(api).find(({ name }) => name === 'husklet_execution_signal');
  assert.equal(signal.inputSchema.safeParse({ id: 'e1', signal: '' }).success, false);
  assert.equal(signal.inputSchema.safeParse({ id: 'e1', signal: 'x'.repeat(33) }).success, false);
  assert.equal(signal.inputSchema.safeParse({ id: 'e1', signal: 'TERM', confirm: true }).success, false);
  await signal.run({ id: 'e1', signal: 'SIGTERM' });
  assert.deepEqual(calls, [['containers.signalExecution', 'e1', 'SIGTERM']]);
});

test('execution removal requires literal confirmation', async () => {
  const { api, calls } = fake();
  const remove = tools(api).find(({ name }) => name === 'husklet_execution_remove');
  assert.equal(remove.inputSchema.safeParse({ id: 'e1' }).success, false);
  assert.equal(remove.inputSchema.safeParse({ id: 'e1', confirm: false }).success, false);
  await remove.run({ id: 'e1', confirm: true });
  assert.deepEqual(calls, [['containers.removeExecution', 'e1']]);
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

test('terminal byte input decodes canonical base64 exactly and refuses ambiguity or overflow before calling', async () => {
  const { api, calls } = fake();
  const write = tools(api).find(({ name }) => name === 'husklet_terminal_write_bytes');
  const exact = Uint8Array.from([0x00, 0x03, 0x1b, 0x7f, 0x80, 0xff]);
  const encoded = Buffer.from(exact).toString('base64');
  assert.equal(write.inputSchema.safeParse({ slot: 'pane-1', input_base64: encoded }).success, true);
  for (const invalid of ['AA', 'AA==\n', 'AA', 'AA-_', 'AB==']) {
    assert.equal(write.inputSchema.safeParse({ slot: 'pane-1', input_base64: invalid }).success, false, invalid);
  }
  const oversized = Buffer.alloc(65_537).toString('base64');
  assert.equal(write.inputSchema.safeParse({ slot: 'pane-1', input_base64: oversized }).success, false);
  await assert.rejects(write.run({ slot: 'pane-1', input_base64: oversized }), /exceeds 65536 bytes/);
  assert.deepEqual(calls, []);
  await write.run({ slot: 'pane-1', input_base64: encoded });
  assert.deepEqual(calls, [['terminal.writeInput', 'pane-1', exact]]);
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
    panes: async () => ({ panes: [{ slot: 'term-1', kind: 'terminal' }], truncated: false }),
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

test('unified pane XML selects surface semantics and gives a clear topology absence error', async () => {
  const terminal = {
    panes: async () => ({ panes: [{ slot: 'surface-1', kind: 'surface' }], truncated: false }),
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
  await assert.rejects(() => paneXml(terminal, 'missing'), /absent from pane inventory/);
});

test('unified pane XML projects arbitrary native slots and explicitly rejects unknown kinds', async () => {
  const terminal = {
    panes: async () => ({ panes: [{ slot: 'settings-native', kind: 'native' }], truncated: false }),
    semantics: async (slot) => ({ slot, revision: 4, truncated: false, root: {
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
    slot, revision: 7, truncated: false,
    root: { id: 0, role: 'column', label: 'A & <B>', value: null, disabled: false, destructive: false, actions: [], children: [
      { id: 3, role: 'button', label: 'Run', value: null, disabled: false, destructive: false, actions: ['invoke'], children: [] },
    ] },
  }; };
  api.terminal.act = async (slot, action) => { calls.push(['terminal.act', slot, action]); };
  const listed = tools(api);
  const snapshot = listed.find(({ name }) => name === 'husklet_pane_snapshot');
  const action = listed.find(({ name }) => name === 'husklet_pane_action');
  const shown = await snapshot.run({ slot: 'pane-1' });
  assert.equal(shown.content[0].text, '<pane slot="pane-1" revision="7" truncated="false"><node id="0" role="column" disabled="false" destructive="false" actions=""><label>A &amp; &lt;B&gt;</label><node id="3" role="button" disabled="false" destructive="false" actions="invoke"><label>Run</label></node></node></pane>');
  await action.run({ slot: 'pane-1', revision: 7, node: 3, action: 'invoke' });
  assert.deepEqual(calls, [
    ['terminal.semantics', 'pane-1'],
    ['terminal.semantics', 'pane-1'],
    ['terminal.act', 'pane-1', { revision: 7, node: 3, action: 'invoke' }],
  ]);
  assert.equal(action.inputSchema.safeParse({ slot: 'pane-1', revision: 7, node: 3, action: 'run' }).success, false);
});

test('pane actions reject stale, absent, disabled and unadvertised controls before dispatch', async () => {
  const { api, calls } = fake();
  api.terminal.semantics = async () => ({ slot: 'pane-1', revision: 12, truncated: false, root: {
    id: 0, role: 'column', label: null, value: null, disabled: false, destructive: false, actions: [], children: [
      { id: 4, role: 'button', label: 'Pending', value: null, disabled: true, destructive: false, actions: [], children: [] },
      { id: 5, role: 'entry', label: 'Name', value: '', disabled: false, destructive: false, actions: ['change'], children: [] },
    ],
  }});
  api.terminal.act = async (...args) => calls.push(['terminal.act', ...args]);
  const action = tools(api).find(({ name }) => name === 'husklet_pane_action');
  await assert.rejects(action.run({ slot: 'pane-1', revision: 11, node: 5, action: 'change' }), /stale semantic revision/);
  await assert.rejects(action.run({ slot: 'pane-1', revision: 12, node: 99, action: 'invoke' }), /is absent/);
  await assert.rejects(action.run({ slot: 'pane-1', revision: 12, node: 4, action: 'invoke' }), /is disabled/);
  await assert.rejects(action.run({ slot: 'pane-1', revision: 12, node: 5, action: 'invoke' }), /does not advertise invoke/);
  assert(!calls.some(([name]) => name === 'terminal.act'));
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

test('execution change wait filters immutable identity and returns subscription credit', async () => {
  const { api } = fake();
  let listener; let disposed = 0;
  api.watchExecutions = async (next) => { listener = next; return async () => { disposed += 1; }; };
  const wait = tools(api).find(({ name }) => name === 'husklet_execution_change_wait');
  const pending = wait.run({ id: 'e2', running: false, timeout_ms: 1000 });
  await new Promise((resolve) => setImmediate(resolve));
  listener({ executions: [{ id: 'e1', running: false }], truncated: false });
  listener({ executions: [{ id: 'e2', running: true }], truncated: false });
  listener({ executions: [{ id: 'e2', running: false, exit_code: 9 }], truncated: true });
  assert.deepEqual(JSON.parse((await pending).content[0].text), {
    changed: true, execution: { id: 'e2', running: false, exit_code: 9 }, truncated: true,
  });
  assert.equal(disposed, 1);
});

test('container change wait filters identity/state and disposes after match', async () => {
  const { api } = fake(); let listener; let disposed = 0;
  api.watchContainers = async (next) => { listener = next; return async () => { disposed += 1; }; };
  const wait = tools(api).find(({ name }) => name === 'husklet_container_change_wait');
  assert.equal(wait.inputSchema.safeParse({ id: 'c1', state: 'running', absent: true }).success, false);
  const pending = wait.run({ id: 'c2', state: 'exited', timeout_ms: 1000 });
  await new Promise((resolve) => setImmediate(resolve));
  listener([{ id: 'c1', state: 'exited' }, { id: 'c2', state: 'running' }]);
  listener([{ id: 'c2', state: 'exited', name: 'worker' }]);
  assert.deepEqual(JSON.parse((await pending).content[0].text), { changed: true, container: { id: 'c2', state: 'exited', name: 'worker' } });
  assert.equal(disposed, 1);
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
      if (name === 'extension_list') return { reply: 'extensions', with: [{ name: 'manager', image_digest: 'sha256:abc', status: 'standby' }] };
      if (name === 'extension_disable') return { reply: 'done' };
      if (name === 'extension_acquisition_start') return { reply: 'extension_acquisition_job', with: { job: 'job-live' } };
      if (name === 'extension_acquisition_status') return { reply: 'extension_acquisition', with: { job: 'job-live', reference: 'example:1', revision: 3, state: 'ready', candidate: { name: 'example', version: '1', image_digest: 'sha256:def', requested: ['interface'] }, error: null } };
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
        slot: argument.slot, revision: 11, truncated: false,
        root: { id: 0, role: 'column', label: 'Live', value: null, disabled: false, actions: ['invoke'], children: [] },
      } };
      if (name === 'pane_semantic_action') return { reply: 'done' };
      if (name === 'terminal_spawn') return { reply: 'done' };
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
  assert.deepEqual(JSON.parse(extensions.content[0].text), [{ name: 'manager', image_digest: 'sha256:abc', status: 'standby' }]);
  await client.callTool({ name: 'husklet_extension_disable', arguments: { name: 'manager', confirm: true } });
  const acquired = await client.callTool({ name: 'husklet_extension_acquire', arguments: { reference: 'example:1', confirm: true } });
  assert.equal(JSON.parse(acquired.content[0].text).job, 'job-live');
  const candidate = await client.callTool({ name: 'husklet_extension_acquisition', arguments: { job: 'job-live' } });
  assert.equal(JSON.parse(candidate.content[0].text).candidate.image_digest, 'sha256:def');
  await client.callTool({ name: 'husklet_extension_install', arguments: { job: 'job-live', revision: 3, granted: ['interface'], confirm: true } });
  const execution = await client.callTool({ name: 'husklet_container_execution', arguments: { id: 'exec-live' } });
  assert.deepEqual(JSON.parse(execution.content[0].text), {
    id: 'exec-live', container_id: 'container-1', running: true, exit_code: null,
  });
  await client.callTool({ name: 'husklet_execution_wait', arguments: { id: 'exec-live', timeout_ms: 250 } });
  await client.callTool({ name: 'husklet_execution_signal', arguments: { id: 'exec-live', signal: 'SIGHUP' } });
  const refusedStop = await client.callTool({ name: 'husklet_container_stop', arguments: { id: 'container-1' } });
  assert.equal(refusedStop.isError, true);
  const refusedKill = await client.callTool({ name: 'husklet_container_kill', arguments: { id: 'container-1', signal: 'SIGKILL' } });
  assert.equal(refusedKill.isError, true);
  await client.callTool({ name: 'husklet_container_stop', arguments: { id: 'container-1', confirm: true } });
  await client.callTool({ name: 'husklet_container_kill', arguments: { id: 'container-1', signal: 'SIGKILL', confirm: true } });
  const images = await client.callTool({ name: 'husklet_image_list', arguments: {} });
  assert.deepEqual(JSON.parse(images.content[0].text), [{ id: 'sha256:abc', references: ['alpine:3.20'], size: 123 }]);
  const volumes = await client.callTool({ name: 'husklet_volume_list', arguments: {} });
  assert.deepEqual(JSON.parse(volumes.content[0].text), [{ name: 'cache', driver: 'local' }]);
  const networks = await client.callTool({ name: 'husklet_network_list', arguments: {} });
  assert.deepEqual(JSON.parse(networks.content[0].text), [{ id: 'n1', name: 'private', driver: 'bridge' }]);
  await client.callTool({ name: 'husklet_terminal_spawn', arguments: {
    slot: 'pane-live', command: ['printf', '%s\n', 'ready'],
  } });
  await client.callTool({ name: 'husklet_terminal_write_bytes', arguments: {
    slot: 'pane-live', input_base64: Buffer.from([0, 3, 0x80, 0xff]).toString('base64'),
  } });
  const snapshot = await client.callTool({ name: 'husklet_pane_snapshot', arguments: { slot: 'pane-live' } });
  assert.match(snapshot.content[0].text, /^<pane slot="pane-live" revision="11"/);
  await client.callTool({ name: 'husklet_pane_action', arguments: { slot: 'pane-live', revision: 11, node: 0, action: 'invoke' } });
  const waited = await client.callTool({ name: 'husklet_pane_wait', arguments: { slot: 'pane-live', timeout_ms: 1000 } });
  assert.deepEqual(JSON.parse(waited.content[0].text).change, {
    slot: 'pane-live', kind: 'surface', revision: 12, generation: 13, coalesced: 2,
  });
  assert.deepEqual(calls, [
    ['workspace_info', undefined],
    ['extension_list', undefined],
    ['extension_disable', { name: 'manager' }],
    ['extension_acquisition_start', { reference: 'example:1' }],
    ['extension_acquisition_status', { job: 'job-live' }],
    ['extension_install', { job: 'job-live', revision: 3, granted: ['interface'] }],
    ['execution_inspect', { id: 'exec-live' }],
    ['execution_wait', { id: 'exec-live', timeout_ms: 250 }],
    ['execution_kill', { id: 'exec-live', signal: 'SIGHUP' }],
    ['container_stop', { id: 'container-1' }],
    ['container_kill', { id: 'container-1', signal: 'SIGKILL' }],
    ['image_list', undefined],
    ['volume_list', undefined],
    ['network_list', undefined],
    ['terminal_spawn', { slot: 'pane-live', command: ['printf', '%s\n', 'ready'] }],
    ['terminal_write_pane', { slot: 'pane-live', contents: [0, 3, 128, 255] }],
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
    if (name === 'pane_list') return { reply: 'panes', with: { panes: [
      { slot: 'term', kind: 'terminal', provider: null, tab: 'tab', title: 'Packed', focused: true },
      { slot: 'surface', kind: 'surface', provider: null, tab: 'tab', title: 'Packed', focused: false },
    ], truncated: false } };
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
    'pane_list', 'terminal_topology', 'terminal_read_pane', 'pane_list', 'pane_semantic_read',
  ]);
  await client.close();
  await server.close();
});

test('pane XML follows every split leaf and refuses a removed stale slot', async () => {
  let changed = false;
  const calls = [];
  const leaf = (slot, focused, columns, rows) => ({
    kind: 'pane', focused, grid: { columns, rows },
    pane: { slot, occupant: 'terminal', working_directory: `/work/${slot}`, command: `shell-${slot}`, provider: null },
  });
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
      slot: argument.slot, lines: [`visible <${argument.slot}>`], truncated: argument.slot === 'upper',
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
    assert.match(xml, new RegExp(`<husklet-pane slot="${slot}" occupant="terminal">`));
    assert.match(xml, new RegExp(`<terminal tab="${tab}"[^>]*active="${active}"[^>]*focused="${focused}"[^>]*columns="${columns}" rows="${rows}"[^>]*truncated="${truncated}">`));
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

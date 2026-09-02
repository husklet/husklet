import assert from 'node:assert/strict';
import net from 'node:net';
import test from 'node:test';

import { ExtensionError, Session, protocolCoverage, workspace } from '../src/index.js';
import { KIND, Reader, encode } from '../src/wire.js';
import { PROTOCOL } from '../src/session.js';

async function pair(options) {
  const server = net.createServer();
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
  const address = server.address();
  const accepted = new Promise((resolve) => server.once('connection', resolve));
  const connecting = new Promise((resolve, reject) => {
    const socket = net.createConnection(address.port, '127.0.0.1');
    socket.once('error', reject);
    socket.once('connect', () => resolve(socket));
  });
  const [host, extension] = await Promise.all([accepted, connecting]);
  host.write(encode({ channel: 0, kind: KIND.request, payload: { protocol: PROTOCOL, extension: 'test', granted: [] } }));
  const session = new Session(extension, options);
  await session.ready;
  return { host, session, server };
}

function frames(stream) {
  const reader = new Reader();
  const queued = [];
  const waiters = [];
  stream.on('data', (chunk) => {
    for (const frame of reader.take(chunk)) {
      const waiter = waiters.shift();
      if (waiter) waiter(frame);
      else queued.push(frame);
    }
  });
  return () => new Promise((resolve) => {
    const frame = queued.shift();
    if (frame) resolve(frame);
    else waiters.push(resolve);
  });
}

test('ordered replies correlate concurrent typed calls and failures reject', async () => {
  const stage = await pair();
  const next = frames(stage.host);
  await next(); // hello
  const api = workspace(stage.session);
  const info = api.info();
  const list = api.containers.list();
  assert.equal((await next()).payload.call, 'workspace_info');
  assert.equal((await next()).payload.call, 'container_list');
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'workspace', with: { name: 'dev', architecture: 'arm64', image: 'alpine' } } }));
  stage.host.write(encode({ channel: 2, kind: KIND.response, flags: 3, payload: { error: 'denied', capability: 'container-read', detail: 'not granted' } }));
  assert.equal((await info).name, 'dev');
  await assert.rejects(list, (error) => error instanceof ExtensionError && error.kind === 'denied');
  stage.session.close(); stage.host.destroy(); stage.server.close();
});

test('pending calls are bounded and a timeout closes the ambiguous ordered stream', async () => {
  const stage = await pair({ pendingLimit: 2, timeout: 100 });
  const next = frames(stage.host);
  await next();
  const first = stage.session.call('workspace_info');
  await new Promise((resolve) => setTimeout(resolve, 60));
  const second = stage.session.call('workspace_list');
  await assert.rejects(stage.session.call('container_list'), /limit/);
  await assert.rejects(first, /timed out/);
  await assert.rejects(second, /timed out/);
  await assert.rejects(stage.session.call('image_list'), /closed/);
  stage.session.close(); stage.host.destroy(); stage.server.close();
});

test('an event returns credit only after delivery', async () => {
  const seen = [];
  const stage = await pair({ onEvent: (event) => seen.push(event) });
  const next = frames(stage.host);
  await next();
  stage.host.write(encode({ channel: 4, kind: KIND.event, payload: { snapshot: 'containers', of: [] } }));
  const credit = await next();
  assert.deepEqual(seen, [{ snapshot: 'containers', of: [] }]);
  assert.equal(credit.channel, 4);
  assert.equal(credit.kind, KIND.credit);
  assert.equal(credit.payload, 1);
  stage.session.close(); stage.host.destroy(); stage.server.close();
});

test('workspace lifecycle methods use the typed control calls', async () => {
  const stage = await pair();
  const next = frames(stage.host);
  await next();
  const api = workspace(stage.session);
  const configuration = {
    name: 'other', image: 'alpine:3.20', architecture: 'arm64', storage: null, shell: null,
    cpus: null, memory_mb: null, environment: [], mounts: [], docker_socket: true,
    scrollback: 100000, vpn: null, execution_lifetime: 'persisted', terminal: {
      font_family: null, font_size: null, foreground: null, background: null,
      cursor_shape: null, cursor_blink: null,
    },
  };
  const operations = [
    api.inspect('other'), api.create(configuration), api.update('other', configuration),
    api.delete('other'), api.start('other'), api.stop('other'), api.restart('other'),
  ];
  const calls = [];
  for (let index = 0; index < operations.length; index += 1) calls.push((await next()).payload);
  assert.deepEqual(calls.map((call) => call.call), [
    'workspace_inspect', 'workspace_create', 'workspace_update', 'workspace_delete',
    'workspace_start', 'workspace_stop', 'workspace_restart',
  ]);
  for (let index = 0; index < operations.length; index += 1) {
    const payload = index < 3
      ? { reply: 'workspace_configuration', with: configuration }
      : { reply: 'done' };
    stage.host.write(encode({ channel: 2, kind: KIND.response, payload }));
  }
  const results = await Promise.all(operations);
  assert.deepEqual(results.slice(0, 3), [configuration, configuration, configuration]);
  assert.deepEqual(results.slice(3), [undefined, undefined, undefined, undefined]);
  stage.session.close(); stage.host.destroy(); stage.server.close();
});

test('coverage names delivered snapshots and leaves unsupported topics unavailable', () => {
  assert.deepEqual(protocolCoverage.available.workspace, [
    'info', 'list', 'inspect', 'create', 'update', 'delete', 'start', 'stop', 'restart',
  ]);
  assert.ok(protocolCoverage.available.containers.includes('create'));
  assert.ok(protocolCoverage.available.containers.includes('remove'));
  assert.ok(protocolCoverage.available.terminal.includes('read'));
  assert.ok(protocolCoverage.available.terminal.includes('split'));
  assert.ok(protocolCoverage.unavailable.workspace.includes('mutateWhileRunning'));
  assert.ok(protocolCoverage.available.containers.includes('processes'));
  assert.deepEqual(protocolCoverage.available.snapshotTopics, ['containers', 'images', 'terminal']);
  assert.ok(protocolCoverage.unavailable.terminal.includes('switchOccupant'));
  assert.ok(protocolCoverage.unavailable.events.includes('volumes'));
  assert.ok(protocolCoverage.unavailable.events.includes('keyboard'));
  const api = workspace({ call() { throw new Error('not called'); } });
  assert.equal(api.renameWorkspace, undefined);
  assert.equal(typeof api.containers.processes, 'function');
  assert.throws(() => api.subscribe('volumes'), /does not publish/);
  assert.equal(typeof api.terminal.writeInput, 'function');
  assert.equal(api.terminal.switchOccupant, undefined);
});

test('deep container methods and subscriptions use exact protocol request shapes', async () => {
  const stage = await pair();
  const next = frames(stage.host);
  await next();
  const api = workspace(stage.session);
  const operations = [
    api.containers.processes('c1'), api.containers.logs('c1', { stdout: true, stderr: false }),
    api.containers.execution('e1'), api.containers.pause('c1'), api.containers.unpause('c1'),
    api.containers.restart('c1'), api.containers.kill('c1', 'SIGTERM'),
    api.containers.exec('c1', { command: ['sh', '-lc', 'true'], user: '1000', workingDirectory: '/work' }),
    api.subscribe('containers'), api.unsubscribe('containers'),
  ];
  const calls = [];
  for (let index = 0; index < operations.length; index += 1) calls.push((await next()).payload);
  assert.deepEqual(calls, [
    { call: 'container_processes', with: { id: 'c1' } },
    { call: 'container_logs', with: { id: 'c1', stdout: true, stderr: false } },
    { call: 'execution_inspect', with: { id: 'e1' } },
    { call: 'container_pause', with: { id: 'c1' } },
    { call: 'container_unpause', with: { id: 'c1' } },
    { call: 'container_restart', with: { id: 'c1' } },
    { call: 'container_kill', with: { id: 'c1', signal: 'SIGTERM' } },
    { call: 'container_exec', with: { id: 'c1', command: ['sh', '-lc', 'true'], user: '1000', working_directory: '/work' } },
    { call: 'event_subscribe', with: { topic: 'containers' } },
    { call: 'event_unsubscribe', with: { topic: 'containers' } },
  ]);
  const replies = [
    { reply: 'processes', with: { titles: [], processes: [] } },
    { reply: 'logs', with: { stdout: [], stderr: [], truncated: false } },
    { reply: 'execution', with: { id: 'e1' } },
    ...Array(4).fill({ reply: 'done' }),
    { reply: 'identity', with: 'e2' },
    { reply: 'done' }, { reply: 'done' },
  ];
  for (const payload of replies) stage.host.write(encode({ channel: 2, kind: KIND.response, payload }));
  const results = await Promise.all(operations);
  assert.equal(results[7], 'e2');
  stage.session.close(); stage.host.destroy(); stage.server.close();
});

test('terminal topology, bounded input and grid resize use exact typed calls', async () => {
  const stage = await pair();
  const next = frames(stage.host);
  await next();
  const terminal = workspace(stage.session).terminal;
  const topology = terminal.topology();
  const writing = terminal.writeInput('s1', 'echo hello\n');
  const resizing = terminal.resizeGrid('s1', 120, 40);
  assert.deepEqual((await next()).payload, { call: 'terminal_topology' });
  assert.deepEqual((await next()).payload, {
    call: 'terminal_write_pane', with: { slot: 's1', contents: [...new TextEncoder().encode('echo hello\n')] },
  });
  assert.deepEqual((await next()).payload, {
    call: 'terminal_resize_grid', with: { slot: 's1', columns: 120, rows: 40 },
  });
  const tree = { active_tab: 't1', tabs: [] };
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'topology', with: tree } }));
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  assert.deepEqual(await topology, tree);
  await Promise.all([writing, resizing]);
  assert.throws(() => terminal.writeInput('s1', new Uint8Array(65_537)), /65536 byte limit/);
  assert.throws(() => terminal.resizeGrid('s1', 0, 24), /1\.\.=1000/);
  stage.session.close(); stage.host.destroy(); stage.server.close();
});

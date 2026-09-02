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

test('a throwing event listener cannot starve healthy listeners or event credit', async () => {
  const seen = [];
  const errors = [];
  const stage = await pair({ onEventError: (error) => errors.push(error.message) });
  const next = frames(stage.host);
  await next();
  stage.session.onEvent(() => { throw new Error('broken observer'); });
  stage.session.onEvent((event) => seen.push(event));
  stage.host.write(encode({ channel: 9, kind: KIND.event, payload: { snapshot: 'images', of: [] } }));
  const credit = await next();
  assert.deepEqual(errors, ['broken observer']);
  assert.deepEqual(seen, [{ snapshot: 'images', of: [] }]);
  assert.deepEqual({ channel: credit.channel, kind: credit.kind, payload: credit.payload }, {
    channel: 9, kind: KIND.credit, payload: 1,
  });
  stage.session.close(); stage.host.destroy(); stage.server.close();
});

test('concurrent pane-change waits share their host subscription until the last disposer', async () => {
  const stage = await pair();
  const next = frames(stage.host);
  await next();
  const api = workspace(stage.session);
  const first = api.watchPaneChanges(() => {});
  const second = api.watchPaneChanges(() => {});
  assert.deepEqual((await next()).payload, { call: 'event_subscribe', with: { topic: 'pane-changes' } });
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  const [stopFirst, stopSecond] = await Promise.all([first, second]);

  await stopFirst();
  const probe = stage.session.call('workspace_info');
  assert.equal((await next()).payload.call, 'workspace_info', 'the first disposer must not unsubscribe the second wait');
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'workspace', with: {} } }));
  await probe;

  const stopped = stopSecond();
  assert.deepEqual((await next()).payload, { call: 'event_unsubscribe', with: { topic: 'pane-changes' } });
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  await stopped;
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
  assert.deepEqual(protocolCoverage.available.snapshotTopics, ['containers', 'executions', 'images', 'volumes', 'networks', 'terminal', 'pane-changes', 'extensions', 'extension-acquisitions', 'workspace-lifecycle', 'workspace-events']);
  assert.ok(protocolCoverage.unavailable.terminal.includes('switchOccupant'));
  assert.ok(!protocolCoverage.unavailable.events.includes('extensions'));
  assert.deepEqual(protocolCoverage.available.extensions, ['list', 'inspect', 'enable', 'disable', 'remove', 'startAcquisition', 'acquisition', 'cancelAcquisition', 'install', 'update']);
  assert.deepEqual(protocolCoverage.unavailable.extensions, []);
  assert.ok(protocolCoverage.available.workspaceEvents.includes('key'));
  const api = workspace({ call() { throw new Error('not called'); } });
  assert.equal(api.renameWorkspace, undefined);
  assert.equal(typeof api.containers.processes, 'function');
  assert.equal(typeof api.volumes.create, 'function');
  assert.equal(typeof api.networks.connect, 'function');
  assert.equal(typeof api.terminal.writeInput, 'function');
  assert.equal(api.terminal.switchOccupant, undefined);
});

test('extension inventory and acquisition watchers use separate exact topics', async () => {
  const stage = await pair(); const next = frames(stage.host); await next(); const api = workspace(stage.session);
  const inventory = []; const acquisitions = [];
  const openingInventory = api.watchExtensions((value) => inventory.push(value));
  assert.deepEqual((await next()).payload, { call: 'event_subscribe', with: { topic: 'extensions' } });
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  const stopInventory = await openingInventory;
  const openingAcquisitions = api.watchExtensionAcquisitions((value) => acquisitions.push(value));
  assert.deepEqual((await next()).payload, { call: 'event_subscribe', with: { topic: 'extension-acquisitions' } });
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  const stopAcquisitions = await openingAcquisitions;
  stage.host.write(encode({ channel: 6, kind: KIND.event, payload: { snapshot: 'extensions', of: [{ name: 'manager', image_digest: 'sha256:a', status: 'duty' }] } }));
  assert.equal((await next()).kind, KIND.credit);
  stage.host.write(encode({ channel: 7, kind: KIND.event, payload: { snapshot: 'extension_acquisitions', of: { job: 'j', revision: 2, state: 'ready', coalesced: 4 } } }));
  assert.equal((await next()).kind, KIND.credit);
  assert.equal(inventory[0][0].name, 'manager'); assert.deepEqual(acquisitions[0], { job: 'j', revision: 2, state: 'ready', coalesced: 4 });
  const stoppingInventory = stopInventory(); assert.deepEqual((await next()).payload.with, { topic: 'extensions' }); stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } })); await stoppingInventory;
  const stoppingAcquisitions = stopAcquisitions(); assert.deepEqual((await next()).payload.with, { topic: 'extension-acquisitions' }); stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } })); await stoppingAcquisitions;
  stage.session.close(); stage.host.destroy(); stage.server.close();
});

test('workspace lifecycle watcher uses its WorkspaceRead-gated exact topic', async () => {
  const stage = await pair(); const next = frames(stage.host); await next(); const api = workspace(stage.session);
  const changes = [];
  const opening = api.watchWorkspaceLifecycle((value) => changes.push(value));
  assert.deepEqual((await next()).payload, { call: 'event_subscribe', with: { topic: 'workspace-lifecycle' } });
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  const stop = await opening;
  stage.host.write(encode({ channel: 8, kind: KIND.event, payload: { snapshot: 'workspace_lifecycle', of: {
    workspace: 'target', action: 'update', revision: 9, coalesced: 2,
  } } }));
  assert.equal((await next()).kind, KIND.credit);
  assert.deepEqual(changes, [{ workspace: 'target', action: 'update', revision: 9, coalesced: 2 }]);
  const stopping = stop();
  assert.deepEqual((await next()).payload, { call: 'event_unsubscribe', with: { topic: 'workspace-lifecycle' } });
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  await stopping;
  stage.session.close(); stage.host.destroy(); stage.server.close();
});

test('execution watcher uses exact topic and returns credit after delivery', async () => {
  const stage = await pair(); const next = frames(stage.host); await next(); const api = workspace(stage.session);
  const seen = [];
  const opening = api.watchExecutions((value) => seen.push(value));
  assert.deepEqual((await next()).payload, { call: 'event_subscribe', with: { topic: 'executions' } });
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  const stop = await opening;
  const catalogue = { executions: [{ id: 'e1', running: false }], truncated: false };
  stage.host.write(encode({ channel: 9, kind: KIND.event, payload: { snapshot: 'executions', of: catalogue } }));
  assert.equal((await next()).kind, KIND.credit); assert.deepEqual(seen, [catalogue]);
  const stopping = stop(); assert.deepEqual((await next()).payload.with, { topic: 'executions' });
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } })); await stopping;
  stage.session.close(); stage.host.destroy(); stage.server.close();
});

test('extension acquisition preserves job revision and explicit grant identity', async () => {
  const stage = await pair(); const next = frames(stage.host); await next();
  const api = workspace(stage.session);
  const operations = [api.extensions.startAcquisition('registry/example:1'), api.extensions.acquisition('job-1'),
    api.extensions.cancelAcquisition('job-1'), api.extensions.install('job-1', 7, ['interface']),
    api.extensions.update('job-2', 8, ['container-read'])];
  const calls = [];
  for (let index = 0; index < operations.length; index += 1) calls.push((await next()).payload);
  assert.deepEqual(calls, [
    { call: 'extension_acquisition_start', with: { reference: 'registry/example:1' } },
    { call: 'extension_acquisition_status', with: { job: 'job-1' } },
    { call: 'extension_acquisition_cancel', with: { job: 'job-1' } },
    { call: 'extension_install', with: { job: 'job-1', revision: 7, granted: ['interface'] } },
    { call: 'extension_update', with: { job: 'job-2', revision: 8, granted: ['container-read'] } },
  ]);
  const summary = { name: 'example', image_digest: 'sha256:abc', status: 'standby' };
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'extension_acquisition_job', with: { job: 'job-1' } } }));
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'extension_acquisition', with: { job: 'job-1', reference: 'registry/example:1', revision: 7, state: 'ready', candidate: null, error: null } } }));
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  for (let index = 0; index < 2; index += 1) stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'extension', with: summary } }));
  const results = await Promise.all(operations);
  assert.equal(results[0].job, 'job-1'); assert.equal(results[1].revision, 7); assert.equal(results[2], undefined);
  assert.deepEqual(results.slice(3), [summary, summary]);
  stage.session.close(); stage.host.destroy(); stage.server.close();
});

test('extension facade preserves exact read and control request shapes', async () => {
  const stage = await pair();
  const next = frames(stage.host);
  await next();
  const api = workspace(stage.session);
  const operations = [api.extensions.list(), api.extensions.inspect('workspace-manager'),
    api.extensions.enable('workspace-manager'), api.extensions.disable('workspace-manager'),
    api.extensions.remove('workspace-manager')];
  const calls = [];
  for (let index = 0; index < operations.length; index += 1) calls.push((await next()).payload);
  assert.deepEqual(calls, [
    { call: 'extension_list' },
    { call: 'extension_inspect', with: { name: 'workspace-manager' } },
    { call: 'extension_enable', with: { name: 'workspace-manager' } },
    { call: 'extension_disable', with: { name: 'workspace-manager' } },
    { call: 'extension_remove', with: { name: 'workspace-manager' } },
  ]);
  const summary = { name: 'workspace-manager', image_digest: 'sha256:abc', status: 'standby' };
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'extensions', with: [summary] } }));
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'extension', with: summary } }));
  for (let index = 0; index < 3; index += 1) stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  assert.deepEqual(await Promise.all(operations), [[summary], summary, undefined, undefined, undefined]);
  stage.session.close(); stage.host.destroy(); stage.server.close();
});

test('volume and network facades preserve safe request shapes', async () => {
  const stage = await pair();
  const next = frames(stage.host);
  await next();
  const api = workspace(stage.session);
  const operations = [
    api.volumes.list(), api.volumes.inspect('cache'), api.volumes.create('cache'), api.volumes.remove('cache'),
    api.networks.list(), api.networks.inspect('private'), api.networks.create('private'),
    api.networks.remove('private'), api.networks.connect('private', 'c1'), api.networks.disconnect('private', 'c1'),
    api.subscribe('volumes'), api.subscribe('networks'), api.subscribe('workspace-events'),
  ];
  const calls = [];
  for (let index = 0; index < operations.length; index += 1) calls.push((await next()).payload);
  assert.deepEqual(calls.map(({ call }) => call), [
    'volume_list', 'volume_inspect', 'volume_create', 'volume_remove', 'network_list', 'network_inspect',
    'network_create', 'network_remove', 'network_connect', 'network_disconnect', 'event_subscribe', 'event_subscribe',
    'event_subscribe',
  ]);
  assert.deepEqual(calls[8].with, { reference: 'private', container: 'c1' });
  assert.deepEqual(calls[9].with, { reference: 'private', container: 'c1' });
  const replies = [
    { reply: 'volumes', with: [] }, { reply: 'volume', with: { name: 'cache', driver: 'local' } },
    { reply: 'volume', with: { name: 'cache', driver: 'local' } }, { reply: 'done' },
    { reply: 'networks', with: [] }, { reply: 'network', with: { id: 'n1', name: 'private', driver: 'bridge', scope: 'local' } },
    { reply: 'identity', with: 'n1' }, ...Array(6).fill({ reply: 'done' }),
  ];
  for (const payload of replies) stage.host.write(encode({ channel: 2, kind: KIND.response, payload }));
  await Promise.all(operations);
  stage.session.close(); stage.host.destroy(); stage.server.close();
});

test('image inspection and destructive calls preserve explicit request shapes', async () => {
  const stage = await pair();
  const next = frames(stage.host);
  await next();
  const api = workspace(stage.session);
  const operations = [api.images.inspect('alpine:3.20'), api.images.remove('alpine:3.20'), api.images.prune()];
  const calls = [];
  for (let index = 0; index < operations.length; index += 1) calls.push((await next()).payload);
  assert.deepEqual(calls, [
    { call: 'image_inspect', with: { reference: 'alpine:3.20' } },
    { call: 'image_remove', with: { reference: 'alpine:3.20' } },
    { call: 'image_prune' },
  ]);
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'image_details', with: { id: 'i1' } } }));
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'image_prune', with: { deleted: 2, space_reclaimed: 7 } } }));
  assert.deepEqual(await Promise.all(operations), [{ id: 'i1' }, undefined, { deleted: 2, space_reclaimed: 7 }]);
  stage.session.close(); stage.host.destroy(); stage.server.close();
});

test('deep container methods and subscriptions use exact protocol request shapes', async () => {
  const stage = await pair();
  const next = frames(stage.host);
  await next();
  const api = workspace(stage.session);
  const operations = [
    api.containers.processes('c1'), api.containers.logs('c1', { stdout: true, stderr: false }),
    api.containers.execution('e1'), api.containers.executions(), api.containers.executionLogs('e1', { stdout: true, stderr: false }), api.containers.waitExecution('e1', { timeoutMs: 250 }), api.containers.pause('c1'), api.containers.unpause('c1'),
    api.containers.restart('c1'), api.containers.kill('c1', 'SIGTERM'), api.containers.signalExecution('e1', 'SIGHUP'), api.containers.removeExecution('e1'),
    api.containers.exec('c1', { command: ['sh', '-lc', 'true'], user: '1000', workingDirectory: '/work' }),
    api.subscribe('containers'), api.unsubscribe('containers'),
  ];
  const calls = [];
  for (let index = 0; index < operations.length - 1; index += 1) calls.push((await next()).payload);
  assert.deepEqual(calls, [
    { call: 'container_processes', with: { id: 'c1' } },
    { call: 'container_logs', with: { id: 'c1', stdout: true, stderr: false } },
    { call: 'execution_inspect', with: { id: 'e1' } },
    { call: 'execution_list' },
    { call: 'execution_logs', with: { id: 'e1', stdout: true, stderr: false } },
    { call: 'execution_wait', with: { id: 'e1', timeout_ms: 250 } },
    { call: 'container_pause', with: { id: 'c1' } },
    { call: 'container_unpause', with: { id: 'c1' } },
    { call: 'container_restart', with: { id: 'c1' } },
    { call: 'container_kill', with: { id: 'c1', signal: 'SIGTERM' } },
    { call: 'execution_kill', with: { id: 'e1', signal: 'SIGHUP' } },
    { call: 'execution_remove', with: { id: 'e1' } },
    { call: 'container_exec', with: { id: 'c1', command: ['sh', '-lc', 'true'], user: '1000', working_directory: '/work' } },
    { call: 'event_subscribe', with: { topic: 'containers' } },
  ]);
  const replies = [
    { reply: 'processes', with: { titles: [], processes: [] } },
    { reply: 'logs', with: { stdout: [], stderr: [], truncated: false } },
    { reply: 'execution', with: { id: 'e1' } },
    { reply: 'executions', with: { executions: [], truncated: false } },
    { reply: 'logs', with: { stdout: [], stderr: [], truncated: false } },
    { reply: 'execution', with: { id: 'e1', running: false, exit_code: 0 } },
    ...Array(6).fill({ reply: 'done' }),
    { reply: 'identity', with: 'e2' },
    { reply: 'done' },
  ];
  for (const payload of replies) stage.host.write(encode({ channel: 2, kind: KIND.response, payload }));
  assert.deepEqual((await next()).payload, { call: 'event_unsubscribe', with: { topic: 'containers' } });
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  const results = await Promise.all(operations);
  assert.equal(results[12], 'e2');
  stage.session.close(); stage.host.destroy(); stage.server.close();
});

test('configured container creation preserves its bounded typed specification', async () => {
  const stage = await pair(); const next = frames(stage.host); await next();
  const spec = {
    image: 'alpine:3.20', name: 'worker', entrypoint: ['/init'], command: ['serve'],
    environment: [['MODE', 'agent']], working_directory: '/work', user: '1000',
    labels: [['owner', 'agent']], mounts: [{ volume: 'cache', target: '/cache', read_only: true }],
    network: 'private', ports: [{ container: 8080, host: 18080, protocol: 'tcp' }],
    memory_mb: 512, cpus: 2, pids_limit: 128,
  };
  const pending = workspace(stage.session).containers.create(spec);
  assert.deepEqual((await next()).payload, { call: 'container_create', with: { spec } });
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'identity', with: 'c-rich' } }));
  assert.equal(await pending, 'c-rich');
  stage.session.close(); stage.host.destroy(); stage.server.close();
});

test('pane change observation subscribes over the live transport, filters metadata, returns credit and disposes', async () => {
  const stage = await pair();
  const next = frames(stage.host);
  await next();
  const api = workspace(stage.session);
  let observed;
  const watching = api.watchPaneChanges((change) => { observed = change; });
  assert.deepEqual((await next()).payload, { call: 'event_subscribe', with: { topic: 'pane-changes' } });
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  const dispose = await watching;
  const change = { slot: 'pane-7', kind: 'surface', revision: 12, generation: 40, coalesced: 3 };
  stage.host.write(encode({ channel: 9, kind: KIND.event, payload: { snapshot: 'pane_changes', of: change } }));
  const credit = await next();
  assert.deepEqual(observed, change);
  assert.equal(credit.channel, 9);
  assert.equal(credit.kind, KIND.credit);
  const stopping = dispose();
  assert.deepEqual((await next()).payload, { call: 'event_unsubscribe', with: { topic: 'pane-changes' } });
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  await stopping;
  stage.session.close(); stage.host.destroy(); stage.server.close();
});

test('terminal topology, bounded input and grid resize use exact typed calls', async () => {
  const stage = await pair();
  const next = frames(stage.host);
  await next();
  const terminal = workspace(stage.session).terminal;
  const topology = terminal.topology();
  const spawning = terminal.spawn('s1', ['printf', '%s\n', 'ready']);
  const writing = terminal.writeInput('s1', 'echo hello\n');
  const resizing = terminal.resizeGrid('s1', 120, 40);
  assert.deepEqual((await next()).payload, { call: 'terminal_topology' });
  assert.deepEqual((await next()).payload, {
    call: 'terminal_spawn', with: { slot: 's1', command: ['printf', '%s\n', 'ready'] },
  });
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
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  assert.deepEqual(await topology, tree);
  await Promise.all([spawning, writing, resizing]);
  assert.throws(() => terminal.spawn('s1', []), /1\.\.=64/);
  assert.throws(() => terminal.spawn('s1', ['sh', 'bad\0argument']), /NUL-free/);
  assert.throws(() => terminal.spawn('s1', ['x'.repeat(4097)]), /4096 bytes/);
  assert.throws(() => terminal.writeInput('s1', new Uint8Array(65_537)), /65536 byte limit/);
  assert.throws(() => terminal.resizeGrid('s1', 0, 24), /1\.\.=1000/);
  stage.session.close(); stage.host.destroy(); stage.server.close();
});

test('pane discovery uses its distinct bounded inventory reply', async () => {
  const stage = await pair();
  const next = frames(stage.host);
  await next();
  const pending = workspace(stage.session).terminal.panes();
  assert.deepEqual((await next()).payload, { call: 'pane_list' });
  const inventory = { panes: [{ slot: 'workspace', kind: 'native', provider: null, tab: null, title: 'Workspace', focused: false }], truncated: false };
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'panes', with: inventory } }));
  assert.deepEqual(await pending, inventory);
  stage.session.close(); stage.host.destroy(); stage.server.close();
});

test('filesystem controls use exact confined protocol request shapes', async () => {
  const stage = await pair();
  const next = frames(stage.host);
  await next();
  const files = workspace(stage.session).files;
  const operations = [files.mkdir('logs/new'), files.rename('logs/a', 'logs/b'), files.remove('logs/b')];
  assert.deepEqual((await next()).payload, { call: 'filesystem_mkdir', with: { path: 'logs/new' } });
  assert.deepEqual((await next()).payload, { call: 'filesystem_rename', with: { from: 'logs/a', to: 'logs/b' } });
  assert.deepEqual((await next()).payload, { call: 'filesystem_remove', with: { path: 'logs/b' } });
  for (let index = 0; index < operations.length; index += 1) {
    stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  }
  await Promise.all(operations);
  stage.session.close(); stage.host.destroy(); stage.server.close();
});

test('pane semantics and actions preserve revision and node identity', async () => {
  const stage = await pair();
  const next = frames(stage.host);
  await next();
  const api = workspace(stage.session);
  const tree = api.terminal.semantics('pane-7');
  const read = (await next()).payload;
  assert.deepEqual(read, { call: 'pane_semantic_read', with: { slot: 'pane-7' } });
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: {
    reply: 'semantics', with: { slot: 'pane-7', revision: 9, truncated: false,
      root: { id: 0, role: 'Column', label: null, value: null, disabled: false, actions: [], children: [] } },
  } }));
  assert.equal((await tree).revision, 9);
  const acted = api.terminal.act('pane-7', { revision: 9, node: 4, action: 'invoke' });
  assert.deepEqual((await next()).payload, { call: 'pane_semantic_action', with: {
    slot: 'pane-7', action: { revision: 9, node: 4, action: 'invoke' },
  } });
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  await acted;
  stage.session.close(); stage.host.destroy(); stage.server.close();
});

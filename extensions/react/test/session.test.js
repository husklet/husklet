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
    api.inspect('other'), api.create(configuration), api.adopt({ ...configuration, generation: '' }), api.update('other', '0123456789abcdef0123456789abcdef', configuration),
    api.delete('other', '0123456789abcdef0123456789abcdef'), api.start('other'), api.stop('other'), api.restart('other'),
  ];
  const calls = [];
  for (let index = 0; index < operations.length; index += 1) calls.push((await next()).payload);
  assert.deepEqual(calls.map((call) => call.call), [
    'workspace_inspect', 'workspace_create', 'workspace_adopt', 'workspace_update', 'workspace_delete',
    'workspace_start', 'workspace_stop', 'workspace_restart',
  ]);
  for (let index = 0; index < operations.length; index += 1) {
    const payload = index < 4
      ? { reply: 'workspace_configuration', with: configuration }
      : { reply: 'done' };
    stage.host.write(encode({ channel: 2, kind: KIND.response, payload }));
  }
  const results = await Promise.all(operations);
  assert.deepEqual(results.slice(0, 4), [configuration, configuration, configuration, configuration]);
  assert.deepEqual(results.slice(4), [undefined, undefined, undefined, undefined]);
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
  assert.deepEqual(protocolCoverage.available.images, [
    'list', 'inspect', 'pull', 'startPull', 'pullStatus', 'cancelPull', 'remove', 'prune',
  ]);
  assert.deepEqual(protocolCoverage.unavailable.images, []);
  assert.deepEqual(protocolCoverage.available.snapshotTopics, ['containers', 'executions', 'images', 'image-pulls', 'volumes', 'networks', 'terminal', 'pane-changes', 'extensions', 'extension-acquisitions', 'workspace-lifecycle', 'workspace-events']);
  assert.ok(protocolCoverage.available.terminal.includes('switchOccupant'));
  assert.ok(!protocolCoverage.unavailable.events.includes('extensions'));
  assert.deepEqual(protocolCoverage.available.extensions, ['list', 'inspect', 'enable', 'disable', 'remove', 'startAcquisition', 'acquisition', 'cancelAcquisition', 'install', 'update']);
  assert.deepEqual(protocolCoverage.unavailable.extensions, []);
  assert.ok(protocolCoverage.available.workspaceEvents.includes('key'));
  const api = workspace({ call() { throw new Error('not called'); } });
  assert.equal(api.renameWorkspace, undefined);
  assert.equal(typeof api.containers.processes, 'function');
  assert.equal(typeof api.volumes.create, 'function');
  assert.equal(typeof api.networks.connect, 'function');
  assert.deepEqual(Object.keys(api.images), protocolCoverage.available.images,
    'coverage must enumerate every callable typed image authority in API order');
  assert.equal(typeof api.terminal.writeInput, 'function');
  assert.equal(typeof api.terminal.switchOccupant, 'function');
});

test('terminal occupant switching validates and preserves the exact CAS wire shape', async () => {
  const stage = await pair(); const next = frames(stage.host); await next(); const api = workspace(stage.session);
  for (const [generation, target] of [
    [-1, { kind: 'terminal' }], [Number.MAX_SAFE_INTEGER + 1, { kind: 'terminal' }],
    [0, { kind: 'terminal', extra: true }], [0, { kind: 'surface', extension: '', provider: 'main' }],
    [0, { kind: 'surface', extension: 'demo', provider: '' }], [0, { kind: 'unknown' }],
  ]) assert.throws(() => api.terminal.switchOccupant('pane-1', generation, target));
  const surface = { kind: 'surface', extension: 'demo', provider: 'main' };
  const first = api.terminal.switchOccupant('pane-1', 7, surface);
  assert.deepEqual((await next()).payload, { call: 'terminal_switch_occupant', with: { slot: 'pane-1', generation: 7, target: surface } });
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } })); await first;
  const second = api.terminal.switchOccupant('pane-1', 8, { kind: 'terminal' });
  assert.deepEqual((await next()).payload, { call: 'terminal_switch_occupant', with: { slot: 'pane-1', generation: 8, target: { kind: 'terminal' } } });
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } })); await second;
  const observed = api.terminal.switchOccupantObserved('pane-1', 9, 12, surface);
  assert.deepEqual((await next()).payload, { call: 'terminal_switch_occupant_observed', with: { slot: 'pane-1', generation: 9, revision: 12, target: surface } });
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } })); await observed;
  assert.throws(() => api.terminal.switchOccupantObserved('pane-1', 9, -1, surface), /generation and revision/);
  stage.session.close(); stage.host.destroy(); stage.server.close();
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

test('workspace input watcher uses its separate grant topic, returns credit, and disposes', async () => {
  const stage = await pair(); const next = frames(stage.host); await next(); const api = workspace(stage.session);
  const batches = [];
  const opening = api.watchWorkspaceEvents((value) => batches.push(value));
  assert.deepEqual((await next()).payload, { call: 'event_subscribe', with: { topic: 'workspace-events' } });
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  const stop = await opening;
  stage.host.write(encode({ channel: 9, kind: KIND.event, payload: { snapshot: 'workspace_events', of: {
    events: [{ event: 'pointer', phase: 'press', slot: 'pane-2', generation: 7, x: 12.5, y: 8, button: 1, modifiers: ['shift'], delta_x: null, delta_y: null }], dropped: 3,
  } } }));
  assert.equal((await next()).kind, KIND.credit);
  assert.equal(batches[0].dropped, 3);
  assert.equal(batches[0].events[0].slot, 'pane-2');
  assert.equal(batches[0].events[0].generation, 7);
  const stopping = stop();
  assert.deepEqual((await next()).payload, { call: 'event_unsubscribe', with: { topic: 'workspace-events' } });
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

test('container watcher uses existing snapshot topic and returns credit after delivery', async () => {
  const stage = await pair(); const next = frames(stage.host); await next(); const api = workspace(stage.session);
  const seen = []; const opening = api.watchContainers((value) => seen.push(value));
  assert.deepEqual((await next()).payload, { call: 'event_subscribe', with: { topic: 'containers' } });
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } })); const stop = await opening;
  const containers = [{ id: 'a'.repeat(64), name: 'worker_2.prod', state: 'running' }];
  stage.host.write(encode({ channel: 10, kind: KIND.event, payload: { snapshot: 'containers', of: containers } }));
  assert.equal((await next()).kind, KIND.credit); assert.deepEqual(seen, [containers]);
  assert.equal(seen[0][0].id, 'a'.repeat(64), 'rename observation preserves immutable identity');
  const stopping = stop(); assert.deepEqual((await next()).payload.with, { topic: 'containers' });
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } })); await stopping;
  stage.session.close(); stage.host.destroy(); stage.server.close();
});

test('image pull jobs and progress watcher preserve exact typed wire shapes', async () => {
  const stage = await pair(); const next = frames(stage.host); await next(); const api = workspace(stage.session);
  const opening = api.watchImagePulls(() => {});
  assert.deepEqual((await next()).payload, { call: 'event_subscribe', with: { topic: 'image-pulls' } });
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } })); const stop = await opening;
  const operations = [api.images.startPull('alpine:3.20'), api.images.pullStatus('7'), api.images.cancelPull('7')];
  assert.deepEqual((await next()).payload, { call: 'image_pull_start', with: { reference: 'alpine:3.20' } });
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'image_pull_job', with: { job: '7' } } }));
  assert.deepEqual((await next()).payload, { call: 'image_pull_status', with: { job: '7' } });
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'image_pull', with: { job: '7' } } }));
  assert.deepEqual((await next()).payload, { call: 'image_pull_cancel', with: { job: '7' } });
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } })); await Promise.all(operations);
  const stopping = stop(); assert.deepEqual((await next()).payload.with, { topic: 'image-pulls' });
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } })); await stopping;
  stage.session.close(); stage.host.destroy(); stage.server.close();
});

test('extension acquisition preserves job revision and explicit grant identity', async () => {
  const stage = await pair(); const next = frames(stage.host); await next();
  const api = workspace(stage.session);
  const operations = [api.extensions.startAcquisition('registry/example:1'), api.extensions.acquisition('job-1'),
    api.extensions.cancelAcquisition('job-1', 7), api.extensions.install('job-1', 7, ['interface', 'container-attach']),
    api.extensions.update('job-2', 8, ['container-read'])];
  const calls = [];
  for (let index = 0; index < operations.length; index += 1) calls.push((await next()).payload);
  assert.deepEqual(calls, [
    { call: 'extension_acquisition_start', with: { reference: 'registry/example:1' } },
    { call: 'extension_acquisition_status', with: { job: 'job-1' } },
    { call: 'extension_acquisition_cancel', with: { job: 'job-1', revision: 7 } },
    { call: 'extension_install', with: { job: 'job-1', revision: 7, granted: ['interface', 'container-attach'] } },
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
    api.extensions.enable('workspace-manager', `sha256:${'a'.repeat(64)}`), api.extensions.disable('workspace-manager', `sha256:${'a'.repeat(64)}`),
    api.extensions.remove('workspace-manager', `sha256:${'a'.repeat(64)}`)];
  const calls = [];
  for (let index = 0; index < operations.length; index += 1) calls.push((await next()).payload);
  assert.deepEqual(calls, [
    { call: 'extension_list' },
    { call: 'extension_inspect', with: { name: 'workspace-manager' } },
    { call: 'extension_enable', with: { name: 'workspace-manager', image_digest: `sha256:${'a'.repeat(64)}` } },
    { call: 'extension_disable', with: { name: 'workspace-manager', image_digest: `sha256:${'a'.repeat(64)}` } },
    { call: 'extension_remove', with: { name: 'workspace-manager', image_digest: `sha256:${'a'.repeat(64)}` } },
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
  const networkId = 'a'.repeat(32);
  const containerId = 'b'.repeat(64);
  assert.throws(() => api.networks.remove('private'), /complete immutable ID/);
  assert.throws(() => api.networks.connect(networkId, 'friendly'), /complete immutable ID/);
  assert.throws(() => api.networks.connect(networkId, containerId, { aliases: ['ok', 'ok'] }), /unique/);
  const aliasOptions = { aliases: ['database.internal', 'database_2'] };
  const operations = [
    api.volumes.list(), api.volumes.inspect('cache'), api.volumes.create('cache'), api.volumes.remove('cache', 'a'.repeat(32)),
    api.networks.list(), api.networks.inspect('private'), api.networks.create('private'),
    api.networks.remove(networkId), api.networks.connect(networkId, containerId), api.networks.connect(networkId, containerId, aliasOptions), api.networks.disconnect(networkId, containerId),
    api.subscribe('volumes'), api.subscribe('networks'), api.subscribe('workspace-events'),
  ];
  const calls = [];
  for (let index = 0; index < operations.length; index += 1) calls.push((await next()).payload);
  assert.deepEqual(calls.map(({ call }) => call), [
    'volume_list', 'volume_inspect', 'volume_create', 'volume_remove', 'network_list', 'network_inspect',
    'network_create', 'network_remove', 'network_connect', 'network_connect', 'network_disconnect', 'event_subscribe', 'event_subscribe',
    'event_subscribe',
  ]);
  assert.deepEqual(calls[3].with, { name: 'cache', generation: 'a'.repeat(32) });
  assert.deepEqual(calls[8].with, { reference: networkId, container: containerId });
  assert.deepEqual(calls[9].with, { reference: networkId, container: containerId, aliases: ['database.internal', 'database_2'] });
  assert.deepEqual(aliasOptions, { aliases: ['database.internal', 'database_2'] });
  assert.deepEqual(calls[10].with, { reference: networkId, container: containerId });
  const replies = [
    { reply: 'volumes', with: [] }, { reply: 'volume', with: { name: 'cache', driver: 'local' } },
    { reply: 'volume', with: { name: 'cache', driver: 'local' } }, { reply: 'done' },
    { reply: 'networks', with: [] }, { reply: 'network', with: { id: 'n1', name: 'private', driver: 'bridge', scope: 'local' } },
    { reply: 'identity', with: 'n1' }, ...Array(7).fill({ reply: 'done' }),
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
  const digest = `sha256:${'a'.repeat(64)}`;
  assert.throws(() => api.images.remove('alpine:3.20'), /complete immutable sha256 digest/);
  const operations = [api.images.inspect('alpine:3.20'), api.images.remove(digest), api.images.prune()];
  const calls = [];
  for (let index = 0; index < operations.length; index += 1) calls.push((await next()).payload);
  assert.deepEqual(calls, [
    { call: 'image_inspect', with: { reference: 'alpine:3.20' } },
    { call: 'image_remove', with: { reference: digest } },
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
  const containerId = 'a'.repeat(64);
  const executionId = 'b'.repeat(32);
  assert.throws(() => api.containers.signalExecution('7', 'SIGTERM'), /complete immutable ID/);
  assert.throws(() => api.containers.removeExecution('execution-name'), /complete immutable ID/);
  await assert.rejects(api.containers.execution('execution-name'), /complete immutable ID/);
  await assert.rejects(api.containers.executionLogs('abc123'), /complete immutable ID/);
  await assert.rejects(api.containers.waitExecution('7'), /complete immutable ID/);
  assert.throws(() => api.containers.stop('friendly-name'), /complete immutable ID/);
  assert.throws(() => api.containers.remove('abc123'), /complete immutable ID/);
  assert.throws(() => api.containers.kill('friendly-name', 'SIGTERM'), /complete immutable ID/);
  assert.throws(() => api.containers.rename('friendly-name', 'worker'), /complete immutable ID/);
  for (const action of ['start', 'pause', 'unpause', 'restart']) {
    assert.throws(() => api.containers[action]('friendly-name'), /complete immutable ID/);
  }
  assert.throws(() => api.containers.rename(containerId, '.worker'), /container name must/);
  assert.throws(() => api.containers.rename(containerId, 'x'.repeat(129)), /container name must/);
  const operations = [
    api.containers.processes('c1'), api.containers.logs('c1', { stdout: true, stderr: false }),
    api.containers.execution(executionId), api.containers.executions(), api.containers.executionLogs(executionId, { stdout: true, stderr: false }), api.containers.waitExecution(executionId, { timeoutMs: 250 }), api.containers.start(containerId), api.containers.pause(containerId), api.containers.unpause(containerId),
    api.containers.restart(containerId), api.containers.rename(containerId, 'worker_2.prod'), api.containers.stop(containerId), api.containers.remove(containerId), api.containers.kill(containerId, 'SIGTERM'), api.containers.signalExecution(executionId, 'SIGHUP'), api.containers.removeExecution(executionId),
    api.containers.exec(containerId, { command: ['sh', '-lc', 'true'], user: '1000', workingDirectory: '/work' }),
    api.subscribe('containers'), api.unsubscribe('containers'),
  ];
  const calls = [];
  for (let index = 0; index < operations.length - 1; index += 1) calls.push((await next()).payload);
  assert.deepEqual(calls, [
    { call: 'container_processes', with: { id: 'c1' } },
    { call: 'container_logs', with: { id: 'c1', stdout: true, stderr: false } },
    { call: 'execution_inspect', with: { id: executionId } },
    { call: 'execution_list' },
    { call: 'execution_logs', with: { id: executionId, stdout: true, stderr: false } },
    { call: 'execution_wait', with: { id: executionId, timeout_ms: 250 } },
    { call: 'container_start', with: { id: containerId } },
    { call: 'container_pause', with: { id: containerId } },
    { call: 'container_unpause', with: { id: containerId } },
    { call: 'container_restart', with: { id: containerId } },
    { call: 'container_rename', with: { id: containerId, name: 'worker_2.prod' } },
    { call: 'container_stop', with: { id: containerId } },
    { call: 'container_remove', with: { id: containerId } },
    { call: 'container_kill', with: { id: containerId, signal: 'SIGTERM' } },
    { call: 'execution_kill', with: { id: executionId, signal: 'SIGHUP' } },
    { call: 'execution_remove', with: { id: executionId } },
    { call: 'container_exec', with: { id: containerId, command: ['sh', '-lc', 'true'], user: '1000', working_directory: '/work' } },
    { call: 'event_subscribe', with: { topic: 'containers' } },
  ]);
  const replies = [
    { reply: 'processes', with: { container_id: containerId, titles: ['PID', 'PPID', 'USER', 'STAT', 'COMMAND'],
      processes: [['1', '0', 'root', '?', '/usr/bin/server']], observed_at_ms: 1_700_000_000_000,
      scope: 'initial', pid_identity: 'snapshot', truncated: false } },
    { reply: 'logs', with: { stdout: [], stderr: [], truncated: false, stdout_truncated: false, stderr_truncated: false, eof: false } },
    { reply: 'execution', with: { id: 'e1' } },
    { reply: 'executions', with: { executions: [], truncated: false } },
    { reply: 'logs', with: { stdout: [], stderr: [], truncated: false, stdout_truncated: false, stderr_truncated: false, eof: true } },
    { reply: 'execution', with: { id: 'e1', running: false, exit_code: 0 } },
    ...Array(10).fill({ reply: 'done' }),
    { reply: 'identity', with: 'e2' },
    { reply: 'done' },
  ];
  for (const payload of replies) stage.host.write(encode({ channel: 2, kind: KIND.response, payload }));
  assert.deepEqual((await next()).payload, { call: 'event_unsubscribe', with: { topic: 'containers' } });
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  const results = await Promise.all(operations);
  assert.deepEqual(
    { containerId: results[0].container_id, scope: results[0].scope, pidIdentity: results[0].pid_identity, truncated: results[0].truncated },
    { containerId, scope: 'initial', pidIdentity: 'snapshot', truncated: false },
  );
  assert.equal(results[1].eof, false, 'empty output from a running initial process remains open');
  assert.deepEqual(
    { eof: results[4].eof, stdout: results[4].stdout_truncated, stderr: results[4].stderr_truncated },
    { eof: true, stdout: false, stderr: false },
  );
  assert.equal(results[16], 'e2');
  stage.session.close(); stage.host.destroy(); stage.server.close();
});

test('configured container creation preserves its bounded typed specification', async () => {
  const stage = await pair(); const next = frames(stage.host); await next();
  const spec = {
    image: 'alpine:3.20', name: 'worker', hostname: 'h'.repeat(253), entrypoint: ['/init'], command: ['serve'],
    environment: [['MODE', 'agent']], working_directory: '/work', user: '1000',
    labels: [['owner', 'agent']], mounts: [{ volume: 'cache', target: '/cache' }],
    network: 'private', ports: [{ container: 8080, host: 18080, protocol: 'tcp' }],
    memory_mb: 512, cpus: 2, pids_limit: 128,
  };
  const pending = workspace(stage.session).containers.create(spec);
  assert.deepEqual((await next()).payload, { call: 'container_create', with: {
    spec: { ...spec, mounts: [{ volume: 'cache', target: '/cache', read_only: false }] },
  } });
  assert.equal(Object.hasOwn(spec.mounts[0], 'read_only'), false, 'normalization does not mutate caller input');
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'identity', with: 'c-rich' } }));
  assert.equal(await pending, 'c-rich');
  stage.session.close(); stage.host.destroy(); stage.server.close();
});

test('container terminal attachment preserves immutable identity and exact argv on the wire', async () => {
  const stage = await pair(); const next = frames(stage.host); await next();
  const id = 'a'.repeat(64);
  const pending = workspace(stage.session).containers.attachTerminal(id, ['sh', '-lc', 'printf "%s" "$HOME"']);
  assert.deepEqual((await next()).payload, {
    call: 'container_attach_terminal', with: { id, command: ['sh', '-lc', 'printf "%s" "$HOME"'] },
  });
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'identity', with: 'p7' } }));
  assert.equal(await pending, 'p7');
  assert.throws(() => workspace(stage.session).containers.attachTerminal('friendly', ['sh']), /complete immutable ID/);
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

test('terminal topology, bounded input, grid resize and retitle use exact typed calls', async () => {
  const stage = await pair();
  const next = frames(stage.host);
  await next();
  const terminal = workspace(stage.session).terminal;
  const topology = terminal.topology();
  const splitting = terminal.splitObserved('s1', 4, 7, 'below');
  const spawning = terminal.spawnObserved('s1', 4, 7, ['printf', '%s\n', 'ready']);
  const writing = terminal.writeInput('s1', 4, 7, 'echo hello\n');
  const resizing = terminal.resizeGrid('s1', 120, 40);
  const retitling = terminal.retitle('s1', ' Build 🧪 ');
  const closing = terminal.closeObserved('s1', 4, 7);
  assert.deepEqual((await next()).payload, { call: 'terminal_topology' });
  assert.deepEqual((await next()).payload, {
    call: 'terminal_split_observed', with: { slot: 's1', generation: 4, revision: 7, division: 'below' },
  });
  assert.deepEqual((await next()).payload, {
    call: 'terminal_spawn_observed', with: { slot: 's1', generation: 4, revision: 7, command: ['printf', '%s\n', 'ready'] },
  });
  assert.deepEqual((await next()).payload, {
    call: 'terminal_write_pane', with: { slot: 's1', generation: 4, revision: 7, contents: [...new TextEncoder().encode('echo hello\n')] },
  });
  assert.deepEqual((await next()).payload, {
    call: 'terminal_resize_grid', with: { slot: 's1', columns: 120, rows: 40 },
  });
  assert.deepEqual((await next()).payload, {
    call: 'terminal_retitle_pane', with: { slot: 's1', title: ' Build 🧪 ' },
  });
  assert.deepEqual((await next()).payload, {
    call: 'terminal_close_pane_observed', with: { slot: 's1', generation: 4, revision: 7 },
  });
  const tree = { active_tab: 't1', tabs: [] };
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'topology', with: tree } }));
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'identity', with: 's2' } }));
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  assert.deepEqual(await topology, tree);
  assert.equal(await splitting, 's2');
  await Promise.all([spawning, writing, resizing, retitling, closing]);
  assert.throws(() => terminal.spawn('s1', []), /1\.\.=64/);
  assert.throws(() => terminal.spawn('s1', ['sh', 'bad\0argument']), /NUL-free/);
  assert.throws(() => terminal.spawn('s1', ['x'.repeat(4097)]), /4096 bytes/);
  assert.throws(() => terminal.spawnObserved('s1', 4, -1, ['true']), /generation and revision/);
  assert.throws(() => terminal.writeInput('s1', 4, 7, new Uint8Array(65_537)), /65536 byte limit/);
  assert.throws(() => terminal.writeInput('s1', -1, 7, 'x'), /generation and revision/);
  assert.throws(() => terminal.closeObserved('s1', 4, -1), /generation and revision/);
  assert.throws(() => terminal.splitObserved('s1', 4, -1, 'below'), /generation and revision/);
  assert.throws(() => terminal.resizeGrid('s1', 0, 24), /1\.\.=1000/);
  for (const title of ['', '   ', 'line\nbreak', 'nul\0byte', '🧪'.repeat(65)]) {
    assert.throws(() => terminal.retitle('s1', title), /pane title must/);
  }
  stage.session.close(); stage.host.destroy(); stage.server.close();
});

test('terminal screen read preserves bounded text, truncation and cursor over framing', async () => {
  const stage = await pair();
  const next = frames(stage.host);
  await next();
  const reading = workspace(stage.session).terminal.read('s1', 25);
  assert.deepEqual((await next()).payload, {
    call: 'terminal_read_pane', with: { slot: 's1', lines: 25 },
  });
  const screen = {
    slot: 's1', generation: 7, revision: 11, columns: 132, rows: 41,
    lines: ['ready'], cursor_column: 5, cursor_row: 2, truncated: true,
  };
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'text', with: screen } }));
  assert.deepEqual(await reading, screen);
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
  const operations = [files.stat('logs/app.log'), files.mkdir('logs/new'), files.rename('logs/a', 'logs/b'), files.remove('logs/b')];
  assert.deepEqual((await next()).payload, { call: 'filesystem_stat', with: { path: 'logs/app.log' } });
  assert.deepEqual((await next()).payload, { call: 'filesystem_mkdir', with: { path: 'logs/new' } });
  assert.deepEqual((await next()).payload, { call: 'filesystem_rename', with: { from: 'logs/a', to: 'logs/b' } });
  assert.deepEqual((await next()).payload, { call: 'filesystem_remove', with: { path: 'logs/b' } });
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'entry', with: { path: 'logs/app.log', directory: false, size: 4 } } }));
  for (let index = 1; index < operations.length; index += 1) {
    stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  }
  const results = await Promise.all(operations); assert.equal(results[0].size, 4);
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
    reply: 'semantics', with: { slot: 'pane-7', generation: 3, revision: 9, truncated: false,
      root: { id: 0, role: 'Column', label: null, value: null, disabled: false, actions: [], children: [] } },
  } }));
  assert.deepEqual({ generation: (await tree).generation, revision: (await tree).revision }, { generation: 3, revision: 9 });
  assert.throws(
    () => api.terminal.act('pane-7', { revision: 9, node: 4, action: 'invoke' }),
    /requires nonnegative safe integer generation/,
  );
  const acted = api.terminal.act('pane-7', { generation: 3, revision: 9, node: 4, action: 'invoke' });
  assert.deepEqual((await next()).payload, { call: 'pane_semantic_action', with: {
    slot: 'pane-7', action: { generation: 3, revision: 9, node: 4, action: 'invoke' },
  } });
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
  await acted;
  stage.session.close(); stage.host.destroy(); stage.server.close();
});

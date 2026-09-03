import assert from 'node:assert/strict';
import test from 'node:test';
import { createElement as h } from 'react';
import { Containers, Executions, Images, Networks, Overview, Processes, Volumes, WorkspaceManager } from '../src/app.js';
import { ContainerDetailsSource, ExecutionDetailsSource, ImageDetailsSource, NetworkDetailsSource, VolumeDetailsSource } from '../src/model.js';
import { host } from './host.js';

const api = {
  containers: { list: async () => [], processes: async () => [], executions: async () => ({ executions: [], truncated: false }) },
  images: { list: async () => [], pull: async () => ({}), inspect: async () => ({}), remove: async () => {}, prune: async () => ({ deleted: 0, space_reclaimed: 0 }) },
  volumes: { list: async () => [], inspect: async () => ({}), create: async () => ({}), remove: async () => {} },
  networks: { list: async () => [], inspect: async () => ({}), create: async () => '', remove: async () => {}, connect: async () => {}, disconnect: async () => {} },
};

test('the test host receives overview and every resource navigation choice', () => {
  const frame = host().render(h(WorkspaceManager, { api, initial: { containers: [], executions: [], images: [], volumes: [], networks: [] } }));
  const labels = frame.patches.filter((patch) => 'SetProp' in patch && patch.SetProp.prop === 'Label').map((patch) => patch.SetProp.value.Text);
  for (const label of ['Workspace overview', 'Containers', 'Processes', 'Executions', 'Images', 'Volumes', 'Networks']) assert.ok(labels.includes(label), label);
  assert.equal(frame.patches.some((patch) => 'Create' in patch && patch.Create.tag === 'Card'), true);
});

test('overview never presents stale inventory counts as current during loading or failure', () => {
  const stale = [{ id: 'old', state: 'running' }];
  const stage = host();
  stage.render(h(Overview, {
    containers: { data: stale, loading: true, error: null },
    images: { data: stale, loading: false, error: new Error('image refresh failed') },
    volumes: { data: [], loading: false, error: null },
    networks: { data: [], loading: false, error: null },
    onOpen: () => {},
  }));
  assert.ok(labelled(stage, '…'));
  assert.ok(labelled(stage, 'Reading inventory…'));
  assert.ok(labelled(stage, 'Unavailable'));
  assert.ok(labelled(stage, 'Refresh failed'));
  assert.equal(labelled(stage, '1 running'), undefined, 'loading cannot retain stale running claims');
  assert.equal(labelled(stage, '1'), undefined, 'failure cannot retain stale inventory counts');
});

test('every empty operational page explains what is absent and how to proceed', async () => {
  const stage = host();
  stage.render(h(WorkspaceManager, { api, initial: { containers: [], executions: [], images: [], volumes: [], networks: [] } }));
  for (const [section, message] of [
    ['Containers', 'No containers'],
    ['Processes', 'No running processes'],
    ['Executions', 'No executions'],
    ['Images', 'No images'],
    ['Volumes', 'No volumes'],
    ['Networks', 'No networks'],
  ]) {
    invoke(stage, section);
    await settled(); await settled();
    assert.ok(labelled(stage, message), `${section} has a semantic empty state`);
  }
});

test('process snapshots disclose initial-only reusable PID scope and host truncation', async () => {
  const processApi = { containers: { processes: async () => ({
    titles: ['PID', 'PPID', 'USER', 'STAT', 'COMMAND'],
    processes: [['1', '0', 'root', '?', '/usr/bin/server']], observed_at_ms: 1_700_000_000_000,
    scope: 'initial', pid_identity: 'snapshot', truncated: true,
  }) } };
  const stage = host();
  stage.render(h(Processes, { api: processApi, resource: { data: [{ id: 'c1', name: 'api' }], loading: false } }));
  await settled(); await settled();
  assert.ok(labelled(stage, 'Initial processes only; PIDs identify this snapshot and may be reused.'));
  assert.ok(labelled(stage, 'Observed 2023-11-14T22:13:20.000Z'));
  assert.ok(labelled(stage, 'The host process snapshot was truncated at its safety limit.'));
  assert.ok(labelled(stage, '/usr/bin/server'));
  assert.ok(!labelled(stage, 'Signal'), 'snapshot PID rows never acquire a control action');
  assert.ok(!labelled(stage, 'Kill'), 'snapshot PID rows never acquire a control action');
});

test('execution observation is scoped to its page and replaces inventory without polling', async () => {
  const calls = [];
  let publish;
  const observed = {
    ...api,
    watchExecutions: async (listener) => {
      calls.push('subscribe');
      publish = listener;
      return async () => calls.push('unsubscribe');
    },
  };
  const stage = host();
  stage.render(h(WorkspaceManager, { api: observed, initial: { containers: [], executions: [], images: [], volumes: [], networks: [] } }));
  assert.deepEqual(calls, []);
  invoke(stage, 'Executions'); await settled();
  assert.deepEqual(calls, ['subscribe']);
  publish({ executions: [{ id: 'live', container_id: 'c1', running: true, exit_code: 0, pid: 9, command: ['live-command'], user: '' }], truncated: true });
  await settled();
  assert.ok(labelled(stage, 'live-command'));
  assert.ok(labelled(stage, 'The host execution catalogue was truncated at its safety limit.'));
  invoke(stage, 'Images'); await settled();
  assert.deepEqual(calls, ['subscribe', 'unsubscribe']);
  publish({ executions: [{ id: 'late', container_id: 'c1', running: false, exit_code: 0, pid: 0, command: ['late-command'], user: '' }], truncated: false });
  await settled();
  assert.equal(labelled(stage, 'late-command'), undefined, 'disposed observation ignores late delivery');
});

test('image removal and prune require an explicit confirmation step', async () => {
  const calls = [];
  const originalDigest = `sha256:${'a'.repeat(64)}`;
  const refreshedDigest = `sha256:${'b'.repeat(64)}`;
  const controlled = { images: {
    ...api.images,
    remove: async (...args) => calls.push(['remove', ...args]),
    prune: async () => { calls.push(['prune']); return { deleted: 0, space_reclaimed: 0 }; },
  } };
  const resource = { data: [{ id: originalDigest, reference: 'alpine:3.20', size: 7, created: 0 }], loading: false, error: null, reload: async () => {} };
  const stage = host();
  const frame = stage.render(h(Images, { api: controlled, resource }));
  const labels = () => stage.frames.flatMap((current) => current.patches).filter((patch) => 'SetProp' in patch && patch.SetProp.prop === 'Label');
  const remove = labels().find((patch) => patch.SetProp.value.Text === 'Remove').SetProp.id;
  assert.ok(stage.surface.dispatch({ trigger: 'Invoke', node: remove, id: `${remove}:Invoke`, value: null }));
  assert.deepEqual(calls, [], 'opening image removal performs no operation');
  assert.ok(labels().some((patch) => patch.SetProp.value.Text === 'Confirm remove'));
  assert.ok(labelled(stage, `Remove immutable image ${originalDigest}?`));
  const staleConfirm = labels().filter((patch) => patch.SetProp.value.Text === 'Confirm remove').at(-1).SetProp.id;
  assert.equal(frame.patches.some((patch) => 'SetProp' in patch && patch.SetProp.value?.Text === 'Confirm remove'), false);
  const refreshed = { ...resource, data: [{ ...resource.data[0], id: refreshedDigest }] };
  stage.render(h(Images, { api: controlled, resource: refreshed }));
  stage.surface.dispatch({ trigger: 'Invoke', node: staleConfirm, id: `${staleConfirm}:Invoke`, value: null });
  await settled();
  assert.deepEqual(calls, [], 'stale digest consent cannot reach removal authority after refresh');
  assert.ok(labelled(stage, `Image ${originalDigest} changed or disappeared; inspect and confirm again.`));
  invoke(stage, 'Remove');
  assert.ok(labelled(stage, `Remove immutable image ${refreshedDigest}?`));
  invoke(stage, 'Confirm remove'); await settled();
  assert.deepEqual(calls, [['remove', refreshedDigest]]);

  const pruneStage = host();
  const pruneFrame = pruneStage.render(h(Images, { api: controlled, resource }));
  const prune = pruneFrame.patches.find((patch) => 'SetProp' in patch && patch.SetProp.prop === 'Label' && patch.SetProp.value.Text === 'Prune unused images').SetProp.id;
  assert.ok(pruneStage.surface.dispatch({ trigger: 'Invoke', node: prune, id: `${prune}:Invoke`, value: null }));
  assert.deepEqual(calls, [['remove', refreshedDigest]], 'opening image prune performs no operation');
  assert.ok(pruneStage.frames.flatMap((current) => current.patches).some((patch) => 'SetProp' in patch && patch.SetProp.value?.Text === 'Confirm prune'));
  invoke(pruneStage, 'Confirm prune');
  await settled();
  assert.deepEqual(calls, [['remove', refreshedDigest], ['prune']]);
});

test('image inspect renders real typed details through a bounded source and retries failures', async () => {
  let attempts = 0;
  const mutations = [];
  const imageDetails = new ImageDetailsSource(async (mutation) => mutations.push(mutation));
  const controlled = { images: {
    ...api.images,
    inspect: async () => {
      attempts += 1;
      if (attempts === 1) throw new Error('manifest temporarily unavailable');
      return { id: 'sha256:one', references: ['alpine:3.20'], created: 'now', size: 7,
        os: 'linux', architecture: 'amd64', entrypoint: ['/bin/sh'], command: [],
        working_directory: '/', user: '' };
    },
  } };
  const resource = { data: [{ id: 'sha256:one', reference: 'alpine:3.20', size: 7 }], loading: false, error: null, reload: async () => {} };
  const stage = host();
  stage.render(h(Images, { api: controlled, resource, imageDetails }));
  invoke(stage, 'Inspect');
  await settled(); await settled();
  assert.ok(labelled(stage, 'Reading image details…'), 'loading is visible and semantic before failure');
  assert.ok(labelled(stage, 'manifest temporarily unavailable'));
  invoke(stage, 'Retry inspect');
  await settled(); await settled();
  assert.equal(attempts, 2);
  assert.ok(labelled(stage, '$.id'), 'image inspection uses the native bounded object projection');
  assert.deepEqual(mutations, [{ Length: { source: 201, version: 1, rows: 9 } }]);
  assert.equal(imageDetails.answer({ source: 201, version: 1, id: 8, range: { start: 0, count: 999 } }).rows.length, 4);
});

test('an empty typed image inspection has an explicit semantic empty state', async () => {
  const controlled = { images: { ...api.images, inspect: async () => ({}) } };
  const resource = { data: [{ id: 'sha256:empty', reference: 'empty:latest', size: 0 }], loading: false, error: null, reload: async () => {} };
  const stage = host();
  stage.render(h(Images, { api: controlled, resource, imageDetails: new ImageDetailsSource() }));
  invoke(stage, 'Inspect');
  await settled(); await settled();
  assert.ok(labelled(stage, 'No image details'));
  assert.ok(stage.frames.flatMap((frame) => frame.patches).some((patch) =>
    patch.SetProp?.prop === 'Detail' && patch.SetProp.value?.Text === 'The host returned no inspectable fields.'));
});

test('structured resource inspection applies the manager hard bounds visibly', async () => {
  const oversized = Object.fromEntries(Array.from({ length: 200 }, (_, index) => [`field_${index}`, `value-${index}`]));
  const controlled = { images: { ...api.images, inspect: async () => ({ id: 'sha256:bounded', ...oversized }) } };
  const resource = { data: [{ id: 'sha256:bounded', reference: 'bounded:latest', size: 1 }], loading: false, error: null, reload: async () => {} };
  const stage = host();
  stage.render(h(Images, { api: controlled, resource, imageDetails: new ImageDetailsSource() }));
  invoke(stage, 'Inspect'); await settled(); await settled();
  assert.ok(labelled(stage, 'Inspection is bounded to 128 nodes, depth 8, and 256 characters per string. Truncated values are marked.'));
  assert.ok(!labelled(stage, '$.field_199'), 'fields beyond the native inspector bound never become nodes');
});

test('image pull progress is determinate, cancellable and retryable from retained input', async () => {
  const calls = []; let publish;
  const controlled = { ...api, images: {
    ...api.images,
    startPull: async (reference) => { calls.push(['start', reference]); return { job: String(calls.length) }; },
    pullStatus: async (job) => ({ job, reference: 'alpine:3.20', revision: 2, state: 'pulling', status: 'Downloading', layer: 'sha256:layer', current: 25, total: 100, image: null, error: null }),
    cancelPull: async (job) => calls.push(['cancel', job]),
  }, watchImagePulls: async (listener) => { publish = listener; return async () => calls.push(['unsubscribe']); } };
  const resource = { data: [], loading: false, error: null, reload: async () => calls.push(['reload']) };
  const stage = host(); stage.render(h(Images, { api: controlled, resource }));
  change(stage, 'registry/image:tag', 'alpine:3.20'); invoke(stage, 'Pull'); await settled(); await settled();
  assert.deepEqual(calls, [['start', 'alpine:3.20']]);
  publish({ job: '1', revision: 2, state: 'pulling', coalesced: 0 }); await settled(); await settled();
  assert.ok(labelled(stage, 'Layer sha256:layer'));
  assert.ok(stage.frames.flatMap((frame) => frame.patches).some((patch) => patch.SetProp?.prop === 'Fraction' && patch.SetProp.value?.Number === 0.25));
  invoke(stage, 'Cancel pull'); await settled(); await settled();
  assert.ok(calls.some((call) => call[0] === 'cancel'));
  assert.ok(stage.frames.flatMap((frame) => frame.patches).some((patch) => patch.SetProp?.prop === 'Detail' && patch.SetProp.value?.Text === 'Pull cancelled.'));
  invoke(stage, 'Pull'); await settled(); await settled();
  assert.deepEqual(calls.filter((call) => call[0] === 'start').map((call) => call[1]), ['alpine:3.20', 'alpine:3.20']);
});

test('a completed image pull reports success, refreshes inventory and retains its reference', async () => {
  const calls = []; let publish;
  const controlled = { ...api, images: { ...api.images,
    startPull: async () => ({ job: 'done' }),
    pullStatus: async () => ({ job: 'done', reference: 'alpine:3.20', revision: 2, state: 'complete', status: 'Pull complete', layer: null, current: 100, total: 100, image: { id: 'i1', reference: 'alpine:3.20', size: 1, created: 0 }, error: null }),
    cancelPull: async () => {},
  }, watchImagePulls: async (listener) => { publish = listener; return async () => {}; } };
  const stage = host(); stage.render(h(Images, { api: controlled, resource: { data: [], loading: false, error: null, reload: async () => calls.push('reload') } }));
  change(stage, 'registry/image:tag', 'alpine:3.20'); invoke(stage, 'Pull'); await settled(); await settled();
  publish({ job: 'done', revision: 2, state: 'complete', coalesced: 0 }); await settled(); await settled();
  assert.ok(labelled(stage, 'Pulled alpine:3.20.')); assert.deepEqual(calls, ['reload']);
  assert.ok(stage.frames.flatMap((frame) => frame.patches).some((patch) => patch.SetProp?.prop === 'Value' && patch.SetProp.value?.Text === 'alpine:3.20'));
});

test('volume and network panels render bounded real inventories and controls', () => {
  const resource = (data) => ({ data, loading: false, error: null, reload: async () => {} });
  const volumeFrame = host().render(h(Volumes, { api, resource: resource([{ name: 'cache', driver: 'local' }]) }));
  const networkFrame = host().render(h(Networks, { api, resource: resource([{ id: 'n1', name: 'private', driver: 'bridge', scope: 'local' }]) }));
  const labels = (frame) => frame.patches.filter((patch) => 'SetProp' in patch && patch.SetProp.prop === 'Label').map((patch) => patch.SetProp.value.Text);
  for (const label of ['Volumes', 'cache', 'Create', 'Inspect', 'Remove']) assert.ok(labels(volumeFrame).includes(label), label);
  for (const label of ['Networks', 'private', 'Connect', 'Disconnect', 'Remove']) assert.ok(labels(networkFrame).includes(label), label);
  const destructive = (frame, label) => {
    const id = frame.patches.find((patch) => 'SetProp' in patch && patch.SetProp.prop === 'Label' && patch.SetProp.value.Text === label).SetProp.id;
    return frame.patches.some((patch) => 'SetProp' in patch && patch.SetProp.id === id && patch.SetProp.prop === 'Destructive' && patch.SetProp.value.Flag === true);
  };
  assert.equal(destructive(volumeFrame, 'Remove'), false);
  assert.equal(destructive(networkFrame, 'Disconnect'), false);
  assert.equal(destructive(networkFrame, 'Remove'), false);
});

test('network inspection exposes loading, retry, empty and bounded typed details', async () => {
  let attempts = 0;
  const controlled = { networks: {
    ...api.networks,
    inspect: async () => {
      attempts += 1;
      if (attempts === 1) throw new Error('network inspect unavailable');
      return { id: 'n1', name: 'private', driver: 'bridge', scope: 'local' };
    },
  } };
  const resource = { data: [{ id: 'n1', name: 'private', driver: 'bridge', scope: 'local' }], loading: false, error: null, reload: async () => {} };
  const details = new NetworkDetailsSource();
  const stage = host();
  stage.render(h(Networks, { api: controlled, resource, networkDetails: details }));
  invoke(stage, 'Inspect'); await settled(); await settled();
  assert.ok(labelled(stage, 'Reading network details…'));
  assert.ok(labelled(stage, 'network inspect unavailable'));
  invoke(stage, 'Retry inspect'); await settled(); await settled();
  assert.ok(labelled(stage, '$.id'), 'network inspection uses the native bounded object projection');
  assert.equal(details.answer({ source: 204, version: 1, id: 1, range: { start: 0, count: 99 } }).rows.length, 4);

  const empty = host();
  empty.render(h(Networks, { api: { networks: { ...api.networks, inspect: async () => ({}) } }, resource, networkDetails: new NetworkDetailsSource() }));
  invoke(empty, 'Inspect'); await settled(); await settled();
  assert.ok(labelled(empty, 'No network details'));
});

test('volume inspection exposes loading, retry, empty and bounded typed details', async () => {
  let attempts = 0;
  const controlled = { volumes: {
    ...api.volumes,
    inspect: async () => {
      attempts += 1;
      if (attempts === 1) throw new Error('volume inspect unavailable');
      return { name: 'cache', driver: 'local' };
    },
  } };
  const resource = { data: [{ name: 'cache', driver: 'local' }], loading: false, error: null, reload: async () => {} };
  const details = new VolumeDetailsSource();
  const stage = host();
  stage.render(h(Volumes, { api: controlled, resource, volumeDetails: details }));
  invoke(stage, 'Inspect'); await settled(); await settled();
  assert.ok(labelled(stage, 'Reading volume details…'));
  assert.ok(labelled(stage, 'volume inspect unavailable'));
  invoke(stage, 'Retry inspect'); await settled(); await settled();
  assert.ok(labelled(stage, '$.name'), 'volume inspection uses the native bounded object projection');
  assert.equal(details.answer({ source: 205, version: 1, id: 1, range: { start: 0, count: 99 } }).rows.length, 2);

  const empty = host();
  empty.render(h(Volumes, { api: { volumes: { ...api.volumes, inspect: async () => ({}) } }, resource, volumeDetails: new VolumeDetailsSource() }));
  invoke(empty, 'Inspect'); await settled(); await settled();
  assert.ok(labelled(empty, 'No volume details'));
});

test('container stop and kill cannot call the API before final confirmation', async () => {
  const calls = [];
  const immutable = 'a'.repeat(64);
  const controlled = {
    containers: {
      inspect: async (id) => ({ id, name: 'api', image: 'alpine', state: 'running', created: 0 }),
      stop: async (...args) => calls.push(['stop', ...args]),
      kill: async (...args) => calls.push(['kill', ...args]),
      exec: async () => {}, logs: async () => new Uint8Array(),
    },
  };
  const resource = {
    data: [{ id: immutable, name: 'api', image: 'alpine', state: 'running' }],
    loading: false, error: null, reload: async () => {},
  };
  const stage = host();
  stage.render(h(Containers, { api: controlled, resource }));

  invoke(stage, 'Stop');
  assert.deepEqual(calls, [], 'opening stop confirmation performs no operation');
  assert.ok(labelled(stage, `Stop api with immutable ID ${immutable}?`));
  assert.equal(isDestructive(stage, 'Confirm stop'), true);
  invoke(stage, 'Cancel');
  assert.deepEqual(calls, [], 'cancelling stop is safe');
  invoke(stage, 'Stop');
  invoke(stage, 'Confirm stop');
  await settled();
  assert.deepEqual(calls, [['stop', immutable]]);

  invoke(stage, 'Details');
  await settled(); await settled();
  invoke(stage, 'Kill');
  assert.deepEqual(calls, [['stop', immutable]], 'opening kill confirmation performs no operation');
  assert.ok(labelled(stage, `Force-kill api with immutable ID ${immutable}?`));
  assert.equal(isDestructive(stage, 'Confirm kill'), true);
  invoke(stage, 'Confirm kill');
  await settled();
  assert.deepEqual(calls.at(-1), ['kill', immutable, 'SIGKILL']);
});

test('container rename validates locally, retries failure, and preserves immutable authority until refresh', async () => {
  const immutable = 'a'.repeat(64);
  const calls = [];
  let attempts = 0;
  const controlled = { containers: {
    rename: async (...args) => {
      calls.push(['rename', ...args]);
      attempts += 1;
      if (attempts === 1) throw new Error('name catalogue temporarily unavailable');
    },
  } };
  const resource = {
    data: [{ id: immutable, name: 'api', image: 'alpine', state: 'running' }],
    loading: false, error: null, reload: async () => calls.push(['reload']),
  };
  const stage = host();
  stage.render(h(Containers, { api: controlled, resource }));
  await settled();
  assert.ok(labelled(stage, `Current name: api. Immutable ID: ${immutable}`));

  change(stage, `New name for ${immutable.slice(0, 12)}`, '.invalid');
  assert.ok(labelled(stage, 'Container name must contain 1–128 ASCII letters, digits, underscores, periods, or hyphens and start with a letter or digit.'));
  assert.equal(isEnabled(stage, 'Rename'), false);
  assert.deepEqual(calls, [], 'invalid input never reaches the typed API');

  change(stage, `New name for ${immutable.slice(0, 12)}`, 'worker_2.prod');
  invoke(stage, 'Rename');
  assert.ok(labelled(stage, 'Renaming…'), 'the in-flight operation is explicit');
  assert.equal(isEnabled(stage, 'Renaming…'), false);
  await settled(); await settled();
  assert.ok(labelled(stage, 'name catalogue temporarily unavailable'));
  assert.ok(labelled(stage, 'api'), 'failed rename does not optimistically replace inventory identity');
  invoke(stage, 'Retry rename'); await settled(); await settled();
  assert.deepEqual(calls, [
    ['rename', immutable, 'worker_2.prod'],
    ['rename', immutable, 'worker_2.prod'],
    ['reload'],
  ]);
  assert.ok(labelled(stage, 'Renamed to worker_2.prod. Inventory identity will update after the authoritative refresh.'));
  assert.ok(labelled(stage, 'api'), 'success notice does not forge an inventory update');
});

test('container creation retains exact identity and retries only start after a partial failure', async () => {
  const calls = [];
  let starts = 0;
  const controlled = { containers: {
    create: async (spec) => { calls.push(['create', spec]); return 'container-new'; },
    start: async (id) => {
      calls.push(['start', id]); starts += 1;
      if (starts === 1) throw new Error('runtime temporarily unavailable');
    },
  } };
  const resource = { data: [], loading: false, error: null, reload: async () => calls.push(['reload']) };
  const stage = host();
  stage.render(h(Containers, { api: controlled, resource }));
  change(stage, 'Image reference', 'alpine:3.20');
  change(stage, 'Container name', 'worker');
  invoke(stage, 'Create and start'); await settled(); await settled();
  assert.ok(labelled(stage, 'runtime temporarily unavailable'));
  assert.ok(labelled(stage, 'Retry start'), 'the exact created container remains recoverable');
  invoke(stage, 'Retry start'); await settled(); await settled();
  assert.ok(labelled(stage, 'Created and started worker.'));
  assert.deepEqual(calls, [
    ['create', { image: 'alpine:3.20', name: 'worker' }],
    ['start', 'container-new'],
    ['start', 'container-new'],
    ['reload'],
  ], 'retry never creates a duplicate container');
});

test('container execution preserves argv and exposes the exact inspectable identity', async () => {
  const calls = [];
  const controlled = { containers: {
    inspect: async () => ({ id: 'container-one', name: 'api', image: 'alpine', state: 'running', created: 0 }),
    exec: async (id, options) => { calls.push(['exec', id, options]); return 'execution-exact-42'; },
    logs: async () => new Uint8Array(),
  } };
  const resource = { data: [{ id: 'container-one', name: 'api', image: 'alpine', state: 'running' }], loading: false, error: null, reload: async () => {} };
  const opened = [];
  const stage = host();
  stage.render(h(Containers, { api: controlled, resource, onOpenExecution: async (id) => opened.push(id) }));
  invoke(stage, 'Details'); await settled(); await settled();

  change(stage, 'Command argv JSON', 'sh -lc echo');
  invoke(stage, 'Execute'); await settled();
  assert.ok(labelled(stage, 'Command must be valid JSON, such as ["sh","-lc","printf hello"].'), 'invalid ambiguous input is rejected');
  assert.deepEqual(calls, []);

  change(stage, 'Command argv JSON', '["sh","-lc","printf hello world"]');
  change(stage, 'Run as user (optional)', '1000:1000');
  change(stage, 'Working directory (optional)', '/workspace with spaces');
  invoke(stage, 'Execute'); await settled(); await settled();
  assert.deepEqual(calls, [['exec', 'container-one', {
    command: ['sh', '-lc', 'printf hello world'], user: '1000:1000', workingDirectory: '/workspace with spaces',
  }]]);
  assert.ok(labelled(stage, 'Runs without an interactive terminal. Inspect the resulting record for status and captured stdout/stderr.'));
  assert.ok(labelled(stage, 'Execution execution-exact-42 created.'));
  invoke(stage, 'Inspect execution'); await settled();
  assert.deepEqual(opened, ['execution-exact-42']);
});

test('container details open an interactive terminal from the same exact argv', async () => {
  const calls = [];
  const id = 'a'.repeat(64);
  const controlled = { containers: {
    inspect: async () => ({ id, name: 'api', image: 'alpine', state: 'running', created: 0 }),
    exec: async () => 'unused',
    attachTerminal: async (...args) => { calls.push(args); return 'p9'; },
    logs: async () => new Uint8Array(),
  } };
  const resource = { data: [{ id, name: 'api', image: 'alpine', state: 'running' }], loading: false, error: null, reload: async () => {} };
  const stage = host();
  stage.render(h(Containers, { api: controlled, resource }));
  invoke(stage, 'Details'); await settled(); await settled();
  change(stage, 'Command argv JSON', '["sh","-lc","printf hello world"]');
  invoke(stage, 'Attach terminal'); await settled(); await settled();
  assert.deepEqual(calls, [[id, ['sh', '-lc', 'printf hello world']]]);
  assert.ok(labelled(stage, 'Interactive terminal opened in p9.'));
});

test('container details load through the bounded source and a failed read is retryable', async () => {
  let attempts = 0;
  const mutations = [];
  const controlled = { containers: {
    inspect: async () => {
      attempts += 1;
      if (attempts === 1) throw new Error('container inspect unavailable');
      return { id: 'container-one', name: 'api', image: 'alpine:3.20', state: 'running', created: 42 };
    },
    exec: async () => {}, logs: async () => new Uint8Array(),
  } };
  const resource = { data: [{ id: 'container-one', name: 'api', image: 'alpine:3.20', state: 'running' }], loading: false, error: null, reload: async () => {} };
  const details = new ContainerDetailsSource(async (mutation) => mutations.push(mutation));
  const stage = host();
  stage.render(h(Containers, { api: controlled, resource, containerDetails: details }));
  invoke(stage, 'Details');
  await settled(); await settled();
  assert.ok(labelled(stage, 'Reading container details…'));
  assert.ok(labelled(stage, 'container inspect unavailable'));
  invoke(stage, 'Retry details');
  await settled(); await settled();
  assert.equal(attempts, 2);
  assert.ok(labelled(stage, '$.id'), 'container inspection uses the native bounded object projection');
  assert.deepEqual(mutations, [{ Length: { source: 202, version: 1, rows: 5 } }]);
  assert.equal(details.answer({ source: 202, version: 1, id: 2, range: { start: 0, count: 999 } }).rows.length, 4);
});

test('empty container inspection remains understandable and leaves quick actions available', async () => {
  const controlled = { containers: { inspect: async () => ({}), exec: async () => {}, logs: async () => new Uint8Array() } };
  const resource = { data: [{ id: 'container-empty', name: 'empty', image: '', state: 'created' }], loading: false, error: null, reload: async () => {} };
  const stage = host();
  stage.render(h(Containers, { api: controlled, resource, containerDetails: new ContainerDetailsSource() }));
  invoke(stage, 'Details');
  await settled(); await settled();
  assert.ok(labelled(stage, 'No container details'));
  assert.ok(labelled(stage, 'Quick actions'), 'empty metadata does not withdraw operational controls');
});

test('execution details, separate bounded streams, wait and retry are operational', async () => {
  const calls = [];
  let inspectAttempts = 0;
  const item = { id: 'e1', container_id: 'c1', running: true, exit_code: 0, pid: 77, command: ['sleep', '5'], user: 'root' };
  const controlled = { containers: {
    execution: async () => { inspectAttempts += 1; if (inspectAttempts === 1) throw new Error('execution moved'); return item; },
    executionLogs: async () => ({ stdout: Array(5_000).fill(111), stderr: [98, 97, 100], truncated: true,
      stdout_truncated: true, stderr_truncated: false, eof: false }),
    waitExecution: async (...args) => { calls.push(['wait', ...args]); return { ...item, running: false, exit_code: 0 }; },
    removeExecution: async () => {},
  } };
  const resource = { data: [item], loading: false, error: null, reload: async () => calls.push(['reload']) };
  const details = new ExecutionDetailsSource();
  const stage = host();
  stage.render(h(Executions, { api: controlled, resource, executionDetails: details }));
  invoke(stage, 'Details'); await settled(); await settled();
  assert.ok(labelled(stage, 'execution moved'));
  invoke(stage, 'Retry details'); await settled(); await settled();
  assert.ok(stage.frames.flatMap((frame) => frame.patches).some((patch) => patch.Create?.tag === 'KeyValueTable'));
  invoke(stage, 'Load output'); await settled(); await settled();
  assert.ok(labelled(stage, 'Standard output')); assert.ok(labelled(stage, 'Standard error'));
  assert.ok(labelled(stage, 'Standard output was truncated to its configured bound.'));
  assert.ok(!labelled(stage, 'Standard error was truncated to its configured bound.'));
  assert.ok(labelled(stage, 'Execution is still running; later output may appear.'));
  const values = stage.frames.flatMap((frame) => frame.patches).filter((patch) => patch.SetProp?.prop === 'Value'
    && typeof patch.SetProp.value?.Text === 'string').map((patch) => patch.SetProp.value.Text);
  assert.ok(values.every((value) => [...value].length <= 4096), 'no LogView patch exceeds its retention bound');
  invoke(stage, 'Wait up to 5s'); await settled(); await settled();
  assert.deepEqual(calls, [['wait', 'e1', { timeoutMs: 5_000 }], ['reload']]);
});

test('finished execution cleanup requires explicit destructive confirmation', async () => {
  const calls = [];
  const item = { id: 'e2', container_id: 'c1', running: false, exit_code: 0, pid: 0, command: ['true'], user: '' };
  const controlled = { containers: {
    execution: async () => item, executionLogs: async () => ({ stdout: [], stderr: [], truncated: false,
      stdout_truncated: false, stderr_truncated: false, eof: true }),
    waitExecution: async () => item, removeExecution: async (...args) => calls.push(args),
  } };
  const resource = { data: [item], loading: false, error: null, reload: async () => {} };
  const stage = host();
  stage.render(h(Executions, { api: controlled, resource }));
  invoke(stage, 'Details'); await settled(); await settled();
  invoke(stage, 'Load output'); await settled(); await settled();
  assert.ok(labelled(stage, 'Captured output is complete (EOF).'));
  invoke(stage, 'Remove record');
  assert.deepEqual(calls, []);
  assert.equal(isDestructive(stage, 'Confirm removal'), true);
  invoke(stage, 'Confirm removal'); await settled(); await settled();
  assert.deepEqual(calls, [['e2']]);
});

test('running execution termination is exact-id, confirmed and refreshes its detail', async () => {
  const calls = [];
  const item = { id: 'execution-full-identity', container_id: 'c1', running: true, exit_code: 0, pid: 42, command: ['sleep', '30'], user: '' };
  const controlled = { containers: {
    execution: async (id) => { calls.push(['inspect', id]); return item; },
    executionLogs: async () => ({ stdout: [], stderr: [], truncated: false }),
    waitExecution: async () => item,
    signalExecution: async (...args) => calls.push(['signal', ...args]),
    removeExecution: async () => {},
  } };
  const resource = { data: [item], loading: false, error: null, reload: async () => calls.push(['reload']) };
  const stage = host();
  stage.render(h(Executions, { api: controlled, resource }));
  invoke(stage, 'Terminate');
  assert.deepEqual(calls, [], 'opening the prompt cannot signal the process');
  assert.ok(labelled(stage, 'Send SIGTERM to execution execution-full-identity?'));
  assert.equal(isDestructive(stage, 'Confirm SIGTERM'), true);
  invoke(stage, 'Confirm SIGTERM'); await settled(); await settled();
  assert.deepEqual(calls, [
    ['signal', 'execution-full-identity', 'SIGTERM'],
    ['reload'],
    ['inspect', 'execution-full-identity'],
  ]);
});

test('empty and host-truncated execution catalogues remain explicit', async () => {
  const item = { id: 'empty', container_id: 'c1', running: false, exit_code: 0, pid: 0, command: [], user: '' };
  const controlled = { containers: {
    execution: async () => ({}), executionLogs: async () => ({ stdout: [], stderr: [], truncated: false }),
    waitExecution: async () => item, removeExecution: async () => {},
  } };
  const resource = { data: [item], loading: false, error: null, reload: async () => {} };
  const stage = host();
  stage.render(h(Executions, { api: controlled, resource, truncated: true }));
  assert.ok(labelled(stage, 'The host execution catalogue was truncated at its safety limit.'));
  invoke(stage, 'Details'); await settled(); await settled();
  assert.ok(labelled(stage, 'Reading execution details…'));
  assert.ok(labelled(stage, 'No execution details'));
});

test('volume and network mutations expose danger only on final confirm and cancel safely', async () => {
  const calls = [];
  const volumeGeneration = 'd'.repeat(32);
  const refreshedVolumeGeneration = 'e'.repeat(32);
  const networkId = 'a'.repeat(32);
  const refreshedNetworkId = 'c'.repeat(32);
  const containerId = 'b'.repeat(64);
  const resource = (data) => ({ data, loading: false, error: null, reload: async () => {} });
  const controlled = {
    volumes: {
      inspect: async () => ({}), create: async () => ({}),
      remove: async (...args) => calls.push(['volume.remove', ...args]),
    },
    networks: {
      inspect: async () => ({}), create: async () => '', connect: async () => {},
      disconnect: async (...args) => calls.push(['network.disconnect', ...args]),
      remove: async (...args) => calls.push(['network.remove', ...args]),
    },
  };

  const volumes = host();
  volumes.render(h(Volumes, { api: controlled, resource: resource([{ name: 'cache', driver: 'local', generation: volumeGeneration }]) }));
  invoke(volumes, 'Remove');
  assert.deepEqual(calls, []);
  assert.equal(isDestructive(volumes, 'Confirm remove'), true);
  assert.ok(labelled(volumes, `Remove volume cache generation ${volumeGeneration}?`));
  const staleVolumeConfirm = labelled(volumes, 'Confirm remove').SetProp.id;
  volumes.render(h(Volumes, { api: controlled, resource: resource([{ name: 'cache', driver: 'local', generation: refreshedVolumeGeneration }]) }));
  volumes.surface.dispatch({ trigger: 'Invoke', node: staleVolumeConfirm, id: `${staleVolumeConfirm}:Invoke`, value: null });
  await settled();
  assert.deepEqual(calls, []);
  invoke(volumes, 'Remove');
  invoke(volumes, 'Confirm remove');
  await settled();
  assert.deepEqual(calls, [['volume.remove', 'cache', refreshedVolumeGeneration]]);

  const networks = host();
  const initialNetworks = resource([{ id: networkId, name: 'private', driver: 'bridge', scope: 'local' }]);
  networks.render(h(Networks, { api: controlled, resource: initialNetworks }));
  change(networks, 'Complete container ID', containerId);
  invoke(networks, 'Disconnect');
  assert.equal(isDestructive(networks, 'Confirm disconnect'), true);
  assert.ok(labelled(networks, `Disconnect immutable container ${containerId} from network ${networkId}?`));
  assert.equal(calls.some(([name]) => name === 'network.disconnect'), false);
  invoke(networks, 'Confirm disconnect');
  await settled();
  assert.deepEqual(calls.at(-1), ['network.disconnect', networkId, containerId]);
  invoke(networks, 'Remove');
  assert.ok(labelled(networks, `Remove immutable network ${networkId} (private)?`));
  assert.equal(calls.some(([name]) => name === 'network.remove'), false);
  const staleConfirm = labelled(networks, 'Confirm remove').SetProp.id;
  const refreshedNetworks = resource([{ id: refreshedNetworkId, name: 'private', driver: 'bridge', scope: 'local' }]);
  networks.render(h(Networks, { api: controlled, resource: refreshedNetworks }));
  networks.surface.dispatch({ trigger: 'Invoke', node: staleConfirm, id: `${staleConfirm}:Invoke`, value: null });
  await settled();
  assert.equal(calls.some(([name]) => name === 'network.remove'), false);
  assert.ok(labelled(networks, `Network ${networkId} changed or disappeared; inspect and confirm again.`));
});

test('shared volume confirmation disables both final actions while removal is pending', async () => {
  let release;
  const controlled = { volumes: {
    ...api.volumes,
    remove: async () => new Promise((resolve) => { release = resolve; }),
  } };
  const resource = {
    data: [{ name: 'cache', driver: 'local', generation: 'd'.repeat(32) }],
    loading: false, error: null, reload: async () => {},
  };
  const stage = host();
  stage.render(h(Volumes, { api: controlled, resource }));
  invoke(stage, 'Remove');
  invoke(stage, 'Confirm remove');
  await settled();
  assert.equal(isEnabled(stage, 'Confirm remove'), false);
  assert.equal(isEnabled(stage, 'Cancel'), false);
  release();
  await settled(); await settled();
  assert.ok(labelled(stage, 'Remove'), 'successful removal closes the shared confirmation');
});

test('volume creation exposes pending failure and retained retry before claiming success', async () => {
  const calls = [];
  let rejectFirst;
  let attempt = 0;
  const controlled = { volumes: {
    ...api.volumes,
    create: async (name) => {
      calls.push(['create', name]); attempt += 1;
      if (attempt === 1) await new Promise((_, reject) => { rejectFirst = reject; });
      return name;
    },
  } };
  const resource = { data: [], loading: false, error: null, reload: async () => calls.push(['reload']) };
  const stage = host(); stage.render(h(Volumes, { api: controlled, resource }));
  change(stage, 'Volume name', ' cache-data '); invoke(stage, 'Create'); await settled();
  assert.ok(labelled(stage, 'Creating volume cache-data…'));
  assert.equal(isEnabled(stage, 'Creating…'), false);
  assert.deepEqual(calls, [['create', 'cache-data']]);
  rejectFirst(new Error(`storage unavailable ${'x'.repeat(600)}`)); await settled(); await settled();
  assert.ok(labelled(stage, 'Retry create'));
  const failures = stage.frames.flatMap((frame) => frame.patches).filter((patch) =>
    patch.SetProp?.prop === 'Label' && patch.SetProp.value?.Text?.startsWith('storage unavailable'));
  assert.equal(failures.at(-1).SetProp.value.Text.length, 513);
  invoke(stage, 'Retry create'); await settled(); await settled();
  assert.deepEqual(calls, [['create', 'cache-data'], ['create', 'cache-data'], ['reload']]);
  assert.ok(labelled(stage, 'Created volume cache-data.'));
});

test('network connect validates aliases, exposes progress, success, bounded failure and retained retry', async () => {
  const calls = [];
  let release;
  let attempt = 0;
  const controlled = { networks: {
    ...api.networks,
    connect: async (...args) => {
      calls.push(args); attempt += 1;
      if (attempt === 1) await new Promise((resolve) => { release = resolve; });
      if (attempt === 2) throw new Error(`temporary ${'x'.repeat(600)}`);
    },
  } };
  const resource = { data: [{ id: 'a'.repeat(32), name: 'private', driver: 'bridge', scope: 'local' }], loading: false, error: null, reload: async () => calls.push(['reload']) };
  const stage = host();
  stage.render(h(Networks, { api: controlled, resource }));

  change(stage, 'Complete container ID', 'friendly');
  change(stage, 'Endpoint aliases (comma-separated, optional)', 'db,db');
  invoke(stage, 'Connect'); await settled();
  assert.deepEqual(calls, [], 'invalid immutable identity and aliases never reach control authority');
  assert.ok(labelled(stage, 'Enter the complete 32- or 64-character lowercase hexadecimal container ID returned by inspection.'));

  change(stage, 'Complete container ID', 'b'.repeat(64));
  invoke(stage, 'Connect'); await settled();
  assert.deepEqual(calls, [], 'duplicate aliases never reach control authority');
  assert.ok(labelled(stage, 'Network endpoint aliases must be at most 64 unique, 1..=253-byte ASCII endpoint names.'));

  change(stage, 'Endpoint aliases (comma-separated, optional)', 'database.internal, database_2');
  invoke(stage, 'Connect'); await settled();
  assert.ok(labelled(stage, 'Connecting immutable endpoint…'));
  assert.deepEqual(calls[0], ['a'.repeat(32), 'b'.repeat(64), { aliases: ['database.internal', 'database_2'] }]);
  release(); await settled(); await settled();
  assert.ok(labelled(stage, `Connected container ${'b'.repeat(64)} to network ${'a'.repeat(32)} with 2 endpoint aliases.`));

  invoke(stage, 'Connect'); await settled(); await settled();
  assert.ok(labelled(stage, 'Retry connect'));
  const errors = stage.frames.flatMap((frame) => frame.patches).filter((patch) => patch.SetProp?.prop === 'Label' && patch.SetProp.value?.Text?.startsWith('temporary '));
  assert.equal(errors.at(-1).SetProp.value.Text.length, 513, 'host failures have a bounded semantic label');
  invoke(stage, 'Retry connect'); await settled(); await settled();
  assert.equal(calls.filter((call) => call[0] === 'a'.repeat(32)).length, 3);
});

test('network creation exposes pending failure and retained retry before claiming success', async () => {
  const calls = [];
  let rejectFirst;
  let attempt = 0;
  const controlled = { networks: {
    ...api.networks,
    create: async (name) => {
      calls.push(['create', name]); attempt += 1;
      if (attempt === 1) await new Promise((_, reject) => { rejectFirst = reject; });
      return 'a'.repeat(32);
    },
  } };
  const resource = { data: [], loading: false, error: null, reload: async () => calls.push(['reload']) };
  const stage = host();
  stage.render(h(Networks, { api: controlled, resource }));
  change(stage, 'Network name', ' private-net ');
  invoke(stage, 'Create'); await settled();
  assert.ok(labelled(stage, 'Creating network private-net…'));
  assert.equal(isEnabled(stage, 'Creating…'), false);
  assert.deepEqual(calls, [['create', 'private-net']]);

  rejectFirst(new Error(`registry unavailable ${'x'.repeat(600)}`));
  await settled(); await settled();
  assert.ok(labelled(stage, 'Retry create'));
  const failures = stage.frames.flatMap((frame) => frame.patches).filter((patch) =>
    patch.SetProp?.prop === 'Label' && patch.SetProp.value?.Text?.startsWith('registry unavailable'));
  assert.equal(failures.at(-1).SetProp.value.Text.length, 513);

  invoke(stage, 'Retry create'); await settled(); await settled();
  assert.deepEqual(calls, [['create', 'private-net'], ['create', 'private-net'], ['reload']]);
  assert.ok(labelled(stage, 'Created network private-net.'));
});

test('disconnect consent snapshots immutable identities and can be cancelled without authority', async () => {
  const calls = [];
  const network = 'a'.repeat(32);
  const first = 'b'.repeat(64);
  const second = 'c'.repeat(64);
  const controlled = { networks: { ...api.networks, disconnect: async (...args) => calls.push(args) } };
  const resource = { data: [{ id: network, name: 'private', driver: 'bridge', scope: 'local' }], loading: false, error: null, reload: async () => {} };
  const stage = host(); stage.render(h(Networks, { api: controlled, resource }));
  change(stage, 'Complete container ID', first); invoke(stage, 'Disconnect');
  assert.ok(labelled(stage, `Disconnect immutable container ${first} from network ${network}?`));
  const staleConfirm = labelled(stage, 'Confirm disconnect').SetProp.id;
  change(stage, 'Complete container ID', second);
  stage.surface.dispatch({ trigger: 'Invoke', node: staleConfirm, id: `${staleConfirm}:Invoke`, value: null });
  await settled();
  assert.deepEqual(calls, [], 'editing identity invalidates prior consent even if a stale event is delivered');
  invoke(stage, 'Disconnect'); invoke(stage, 'Cancel'); await settled();
  assert.deepEqual(calls, []);
  invoke(stage, 'Disconnect'); invoke(stage, 'Confirm disconnect'); await settled(); await settled();
  assert.deepEqual(calls, [[network, second]]);
  assert.ok(labelled(stage, `Disconnected container ${second} from network ${network}.`));
});

test('a failed final confirmation stays visible and retryable', async () => {
  let attempts = 0;
  const controlled = { volumes: {
    inspect: async () => ({}), create: async () => ({}),
    remove: async () => { attempts += 1; throw new Error('volume remains in use'); },
  } };
  const resource = { data: [{ name: 'cache', driver: 'local', generation: 'e'.repeat(32) }], loading: false, error: null, reload: async () => {} };
  const stage = host();
  stage.render(h(Volumes, { api: controlled, resource }));
  invoke(stage, 'Remove');
  invoke(stage, 'Confirm remove');
  await settled();

  assert.equal(attempts, 1);
  assert.ok(labelled(stage, 'volume remains in use'), 'the semantic tree carries the bounded failure');
  assert.equal(isDestructive(stage, 'Confirm remove'), true, 'the final action remains available for retry');
  invoke(stage, 'Cancel');
  assert.equal(attempts, 1, 'cancelling after failure does not retry');
});

test('stale volume generation refuses authority and remains visibly retryable', async () => {
  const calls = [];
  const oldGeneration = 'd'.repeat(32);
  const currentGeneration = 'e'.repeat(32);
  const controlled = { volumes: {
    inspect: async () => ({}), create: async () => ({}),
    remove: async (...args) => calls.push(args),
  } };
  const resource = { data: [
    { name: 'cache', driver: 'local', generation: oldGeneration },
    { name: 'cache', driver: 'local', generation: currentGeneration },
  ], loading: false, error: null, reload: async () => {} };
  const stage = host(); stage.render(h(Volumes, { api: controlled, resource }));
  const removes = stage.frames.flatMap((frame) => frame.patches).filter((patch) =>
    patch.SetProp?.prop === 'Label' && patch.SetProp.value?.Text === 'Remove');
  assert.ok(stage.surface.dispatch({ trigger: 'Invoke', node: removes[0].SetProp.id, id: `${removes[0].SetProp.id}:Invoke`, value: null }));
  invoke(stage, 'Confirm remove'); await settled(); await settled();
  assert.deepEqual(calls, []);
  assert.ok(labelled(stage, 'Volume cache changed generation; inspect and confirm again.'));
  assert.equal(isDestructive(stage, 'Confirm remove'), true);
});

function labelled(stage, label) {
  return stage.frames.flatMap((frame) => frame.patches).filter((patch) =>
    'SetProp' in patch && patch.SetProp.prop === 'Label' && patch.SetProp.value?.Text === label).at(-1);
}

function invoke(stage, label) {
  const nodes = stage.frames.flatMap((frame) => frame.patches).filter((patch) =>
    'SetProp' in patch && patch.SetProp.prop === 'Label' && patch.SetProp.value?.Text === label)
    .map((patch) => patch.SetProp.id).reverse();
  assert.ok(nodes.length, `${label} is visible`);
  assert.ok(nodes.some((node) => stage.surface.dispatch({ trigger: 'Invoke', node, id: `${node}:Invoke`, value: null })), `${label} invokes`);
}

function change(stage, placeholder, value) {
  const node = stage.frames.flatMap((frame) => frame.patches).filter((patch) =>
    'SetProp' in patch && patch.SetProp.prop === 'Placeholder' && patch.SetProp.value?.Text === placeholder).at(-1)?.SetProp.id;
  assert.notEqual(node, undefined, `${placeholder} field is visible`);
  assert.ok(stage.surface.dispatch({ trigger: 'Change', node, id: `${node}:Change`, value }), `${placeholder} changes`);
}

function isDestructive(stage, label) {
  const node = labelled(stage, label)?.SetProp.id;
  return stage.frames.flatMap((frame) => frame.patches).some((patch) =>
    'SetProp' in patch && patch.SetProp.id === node && patch.SetProp.prop === 'Destructive' && patch.SetProp.value?.Flag === true);
}

function isEnabled(stage, label) {
  const node = labelled(stage, label)?.SetProp.id;
  return stage.frames.flatMap((frame) => frame.patches).filter((patch) =>
    'SetProp' in patch && patch.SetProp.id === node && patch.SetProp.prop === 'Enabled').at(-1)?.SetProp.value?.Flag;
}

const settled = () => new Promise((resolve) => setImmediate(resolve));

import assert from 'node:assert/strict';
import test from 'node:test';
import { createElement as h } from 'react';
import { Containers, Executions, Images, Networks, Volumes, WorkspaceManager } from '../src/app.js';
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
  const controlled = { images: {
    ...api.images,
    remove: async (...args) => calls.push(['remove', ...args]),
    prune: async () => { calls.push(['prune']); return { deleted: 0, space_reclaimed: 0 }; },
  } };
  const resource = { data: [{ id: 'sha256:one', reference: 'alpine:3.20', size: 7, created: 0 }], loading: false, error: null, reload: async () => {} };
  const stage = host();
  const frame = stage.render(h(Images, { api: controlled, resource }));
  const labels = () => stage.frames.flatMap((current) => current.patches).filter((patch) => 'SetProp' in patch && patch.SetProp.prop === 'Label');
  const remove = labels().find((patch) => patch.SetProp.value.Text === 'Remove').SetProp.id;
  assert.ok(stage.surface.dispatch({ trigger: 'Invoke', node: remove, id: `${remove}:Invoke`, value: null }));
  assert.deepEqual(calls, [], 'opening image removal performs no operation');
  assert.ok(labels().some((patch) => patch.SetProp.value.Text === 'Confirm remove'));
  assert.equal(frame.patches.some((patch) => 'SetProp' in patch && patch.SetProp.value?.Text === 'Confirm remove'), false);
  invoke(stage, 'Cancel');
  assert.deepEqual(calls, [], 'cancelling image removal is safe');

  const pruneStage = host();
  const pruneFrame = pruneStage.render(h(Images, { api: controlled, resource }));
  const prune = pruneFrame.patches.find((patch) => 'SetProp' in patch && patch.SetProp.prop === 'Label' && patch.SetProp.value.Text === 'Prune unused images').SetProp.id;
  assert.ok(pruneStage.surface.dispatch({ trigger: 'Invoke', node: prune, id: `${prune}:Invoke`, value: null }));
  assert.deepEqual(calls, [], 'opening image prune performs no operation');
  assert.ok(pruneStage.frames.flatMap((current) => current.patches).some((patch) => 'SetProp' in patch && patch.SetProp.value?.Text === 'Confirm prune'));
  invoke(pruneStage, 'Confirm prune');
  await settled();
  assert.deepEqual(calls, [['prune']]);
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
  assert.ok(stage.frames.flatMap((frame) => frame.patches).some((patch) => patch.Create?.tag === 'KeyValueTable'));
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
  assert.ok(stage.frames.flatMap((frame) => frame.patches).some((patch) => patch.Create?.tag === 'KeyValueTable'));
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
  assert.ok(stage.frames.flatMap((frame) => frame.patches).some((patch) => patch.Create?.tag === 'KeyValueTable'));
  assert.equal(details.answer({ source: 205, version: 1, id: 1, range: { start: 0, count: 99 } }).rows.length, 2);

  const empty = host();
  empty.render(h(Volumes, { api: { volumes: { ...api.volumes, inspect: async () => ({}) } }, resource, volumeDetails: new VolumeDetailsSource() }));
  invoke(empty, 'Inspect'); await settled(); await settled();
  assert.ok(labelled(empty, 'No volume details'));
});

test('container stop and kill cannot call the API before final confirmation', async () => {
  const calls = [];
  const controlled = {
    containers: {
      inspect: async (id) => ({ id, name: 'api', image: 'alpine', state: 'running', created: 0 }),
      stop: async (...args) => calls.push(['stop', ...args]),
      kill: async (...args) => calls.push(['kill', ...args]),
      exec: async () => {}, logs: async () => new Uint8Array(),
    },
  };
  const resource = {
    data: [{ id: 'container-one', name: 'api', image: 'alpine', state: 'running' }],
    loading: false, error: null, reload: async () => {},
  };
  const stage = host();
  stage.render(h(Containers, { api: controlled, resource }));

  invoke(stage, 'Stop');
  assert.deepEqual(calls, [], 'opening stop confirmation performs no operation');
  assert.equal(isDestructive(stage, 'Confirm stop'), true);
  invoke(stage, 'Cancel');
  assert.deepEqual(calls, [], 'cancelling stop is safe');
  invoke(stage, 'Stop');
  invoke(stage, 'Confirm stop');
  await settled();
  assert.deepEqual(calls, [['stop', 'container-one']]);

  invoke(stage, 'Details');
  await settled(); await settled();
  invoke(stage, 'Kill');
  assert.deepEqual(calls, [['stop', 'container-one']], 'opening kill confirmation performs no operation');
  assert.equal(isDestructive(stage, 'Confirm kill'), true);
  invoke(stage, 'Confirm kill');
  await settled();
  assert.deepEqual(calls.at(-1), ['kill', 'container-one', 'SIGKILL']);
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
  assert.ok(stage.frames.flatMap((frame) => frame.patches).some((patch) => patch.Create?.tag === 'KeyValueTable'));
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
    executionLogs: async () => ({ stdout: Array(5_000).fill(111), stderr: [98, 97, 100], truncated: true }),
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
  assert.ok(labelled(stage, 'Host output was truncated to its configured bound.'));
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
    execution: async () => item, executionLogs: async () => ({ stdout: [], stderr: [], truncated: false }),
    waitExecution: async () => item, removeExecution: async (...args) => calls.push(args),
  } };
  const resource = { data: [item], loading: false, error: null, reload: async () => {} };
  const stage = host();
  stage.render(h(Executions, { api: controlled, resource }));
  invoke(stage, 'Remove record');
  assert.deepEqual(calls, []);
  assert.equal(isDestructive(stage, 'Confirm removal'), true);
  invoke(stage, 'Confirm removal'); await settled(); await settled();
  assert.deepEqual(calls, [['e2']]);
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
  volumes.render(h(Volumes, { api: controlled, resource: resource([{ name: 'cache', driver: 'local' }]) }));
  invoke(volumes, 'Remove');
  assert.deepEqual(calls, []);
  assert.equal(isDestructive(volumes, 'Confirm remove'), true);
  invoke(volumes, 'Cancel');
  assert.deepEqual(calls, []);
  invoke(volumes, 'Remove');
  invoke(volumes, 'Confirm remove');
  await settled();
  assert.deepEqual(calls, [['volume.remove', 'cache']]);

  const networks = host();
  networks.render(h(Networks, { api: controlled, resource: resource([{ id: 'n1', name: 'private', driver: 'bridge', scope: 'local' }]) }));
  change(networks, 'Container ID for connect/disconnect', 'c1');
  invoke(networks, 'Disconnect');
  assert.equal(isDestructive(networks, 'Confirm disconnect'), true);
  assert.equal(calls.some(([name]) => name === 'network.disconnect'), false);
  invoke(networks, 'Confirm disconnect');
  await settled();
  assert.deepEqual(calls.at(-1), ['network.disconnect', 'n1', 'c1']);
  invoke(networks, 'Remove');
  assert.equal(calls.some(([name]) => name === 'network.remove'), false);
  invoke(networks, 'Cancel');
  assert.equal(calls.some(([name]) => name === 'network.remove'), false);
});

test('a failed final confirmation stays visible and retryable', async () => {
  let attempts = 0;
  const controlled = { volumes: {
    inspect: async () => ({}), create: async () => ({}),
    remove: async () => { attempts += 1; throw new Error('volume remains in use'); },
  } };
  const resource = { data: [{ name: 'cache', driver: 'local' }], loading: false, error: null, reload: async () => {} };
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

const settled = () => new Promise((resolve) => setImmediate(resolve));

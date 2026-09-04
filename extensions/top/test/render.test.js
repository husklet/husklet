import assert from 'node:assert/strict';
import test from 'node:test';
import { createElement as h } from 'react';
import { Containers, Executions, Images, Networks, Overview, Processes, Terminals, Volumes, Top } from '../dist/app.js';
import { ContainerDetailsSource, ExecutionDetailsSource, ImageDetailsSource, NetworkDetailsSource, VolumeDetailsSource } from '../dist/model.js';
import { host } from './host.js';

const api = {
  containers: { list: async () => [], processes: async () => [], executions: async () => ({ executions: [], truncated: false }) },
  images: { list: async () => [], pull: async () => ({}), inspect: async () => ({}), remove: async () => {}, prune: async () => ({ deleted: 0, space_reclaimed: 0 }) },
  volumes: { list: async () => [], inspect: async () => ({}), create: async () => ({}), remove: async () => {} },
  networks: { list: async () => [], inspect: async () => ({}), create: async () => '', remove: async () => {}, connect: async () => {}, disconnect: async () => {} },
  terminal: { tabs: async () => [], pinTab: async () => {}, focus: async () => {} },
};

test('the test host receives overview and every resource navigation choice', () => {
  const frame = host().render(h(Top, { api, initial: { containers: [], executions: [], images: [], volumes: [], networks: [] } }));
  const labels = frame.patches.filter((patch) => 'SetProp' in patch && patch.SetProp.prop === 'Label').map((patch) => patch.SetProp.value.Text);
  for (const label of ['Top', 'Resource overview', 'Containers', 'Processes', 'Executions', 'Images', 'Volumes', 'Networks', 'Terminals']) assert.ok(labels.includes(label), label);
  assert.equal(labels.includes('Workspace'), false, 'the resource manager does not impersonate the Workspace settings extension');
  assert.equal(frame.patches.some((patch) => 'Create' in patch && patch.Create.tag === 'Card'), true);
  assert.equal(property(stageFromFrame(frame), 'Overview', 'Variant')?.Variant, 'Filled');
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

test('late inventory reloads cannot replace a newer authoritative snapshot', async () => {
  const pending = []; let publish;
  const controlled = { ...api, containers: { ...api.containers, list: () => new Promise((resolve) => pending.push(resolve)) } };
  const selections = { subscribe(listener) { publish = listener; return () => {}; } };
  const stage = host();
  stage.render(h(Top, { api: controlled, selections, initial: { containers: [], executions: [], images: [], volumes: [], networks: [] } }));
  await settled();
  publish({ snapshot: 'containers' }); publish({ snapshot: 'containers' });
  await settled();
  pending[1]([{ id: 'new-a', state: 'running' }, { id: 'new-b', state: 'running' }]);
  await settled(); await settled();
  assert.ok(labelled(stage, '2'));
  assert.ok(labelled(stage, '2 running'));
  pending[0]([{ id: 'stale', state: 'stopped' }]);
  await settled(); await settled();
  assert.ok(labelled(stage, '2'));
  assert.ok(labelled(stage, '2 running'));
  assert.equal(labelled(stage, '1'), undefined, 'superseded inventory authority never reaches the overview');
});

test('every empty operational page explains what is absent and how to proceed', async () => {
  const stage = host();
  stage.render(h(Top, { api, initial: { containers: [], executions: [], images: [], volumes: [], networks: [] } }));
  for (const [section, message] of [
    ['Containers', 'No containers'],
    ['Processes', 'No running processes'],
    ['Executions', 'No executions'],
    ['Images', 'No images'],
    ['Volumes', 'No volumes'],
    ['Networks', 'No networks'],
    ['Terminals', 'No terminal tabs'],
  ]) {
    invoke(stage, section);
    await settled(); await settled();
    assert.ok(labelled(stage, message), `${section} has a semantic empty state`);
  }
});

test('terminal management exposes exact pin state and acts through immutable tab identity', async () => {
  const calls = [];
  const resource = {
    data: [{ id: 'p7', title: 'Build', pinned: false, panes: [{ slot: 's4', occupant: 'terminal', provider: null }] }],
    loading: false,
    error: null,
    reload: async () => calls.push(['reload']),
  };
  const stage = host();
  stage.render(h(Terminals, { api: { terminal: {
    pinTab: async (...args) => calls.push(['pin', ...args]),
    focus: async (...args) => calls.push(['focus', ...args]),
  } }, resource }));
  assert.ok(labelled(stage, 'Unpinned'));
  assert.ok(labelled(stage, 's4 · terminal'));
  invoke(stage, 'Pin Build');
  await settled(); await settled();
  assert.deepEqual(calls, [['pin', 'p7', true], ['reload']]);
  invoke(stage, 'Focus Build');
  await settled();
  assert.deepEqual(calls.at(-1), ['focus', 's4']);
});

test('terminal management reads every pane as text and writes against the inspected terminal revision', async () => {
  const calls = [];
  const terminal = {
    toText: async (slot) => {
      calls.push(['read', slot]);
      if (slot === 'pane-ui') return { kind: 'ui', text: '<pane><button label="Deploy"/></pane>', snapshot: { slot, generation: 3, revision: 4 } };
      return { kind: 'terminal', text: '$ ready', snapshot: { slot, generation: 7, revision: 11, lines: ['$ ready'], truncated: false } };
    },
    writeAndWait: async (...args) => {
      calls.push(['write', ...args]);
      return { changed: true, after: { slot: args[0], generation: 7, revision: 12, lines: ['$ ready', 'hello'], truncated: false } };
    },
    pinTab: async () => {}, focus: async () => {},
  };
  const resource = {
    data: [{ id: 'tab-1', title: 'Shell', pinned: false, panes: [
      { slot: 'pane-1', occupant: 'terminal', provider: null },
      { slot: 'pane-ui', occupant: 'surface', provider: { extension: 'postgres', provider: 'overview' } },
    ] }],
    loading: false, error: null, reload: async () => {},
  };
  const stage = host();
  stage.render(h(Terminals, { api: { terminal }, resource }));
  invoke(stage, 'Inspect pane-1'); await settled(); await settled();
  assert.deepEqual(calls, [['read', 'pane-1']]);
  assert.equal(latestPropertyForTag(stage, 'LogView', 'Value')?.Text, '$ ready');
  change(stage, 'Send a line to this terminal', 'printf hello');
  invoke(stage, 'Send line'); await settled(); await settled();
  assert.deepEqual(calls[1], ['write', 'pane-1', 7, 11, 'printf hello\n', { lines: 200 }]);
  assert.equal(latestPropertyForTag(stage, 'LogView', 'Value')?.Text, '$ ready\nhello');
  assert.equal(fieldValue(stage, 'Send a line to this terminal'), '');
  invoke(stage, 'Inspect pane-ui'); await settled(); await settled();
  assert.equal(latestPropertyForTag(stage, 'LogView', 'Value')?.Text, '<pane><button label="Deploy"/></pane>');
  assert.ok(labelled(stage, 'Interface pane-ui'));
  assert.deepEqual(calls.filter(([kind]) => kind === 'write').length, 1, 'reading semantic XML never writes terminal bytes');
});

test('terminal input stays unavailable without a host-issued revision cursor', async () => {
  const calls = [];
  const resource = {
    data: [{ id: 'tab-1', title: 'Shell', pinned: false, panes: [{ slot: 'pane-1', occupant: 'terminal', provider: null }] }],
    loading: false, error: null, reload: async () => {},
  };
  const stage = host();
  stage.render(h(Terminals, { api: { terminal: {
    toText: async () => ({ kind: 'terminal', text: '$ old host', snapshot: { slot: 'pane-1', lines: ['$ old host'], truncated: false } }),
    writeAndWait: async (...args) => calls.push(args), pinTab: async () => {}, focus: async () => {},
  } }, resource }));
  invoke(stage, 'Inspect pane-1'); await settled(); await settled();
  assert.ok(labelled(stage, 'This host did not provide a writable pane revision; refresh before sending input.'));
  assert.equal(isEnabled(stage, 'Send line'), false);
  assert.deepEqual(calls, [], 'input without an observed generation and revision cannot reach the socket');
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

test('one unavailable container does not hide healthy process snapshots', async () => {
  const processApi = { containers: { processes: async (id) => {
    if (id === 'broken') throw new Error('container is stopped');
    return {
      titles: ['PID', 'COMMAND'], processes: [['17', '/usr/bin/healthy']],
      observed_at_ms: 1_700_000_000_000, scope: 'namespace', pid_identity: 'snapshot', truncated: false,
    };
  } } };
  const stage = host();
  stage.render(h(Processes, { api: processApi, resource: {
    data: [{ id: 'healthy', name: 'api' }, { id: 'broken', name: 'worker' }],
    loading: false, error: null, reload: async () => {},
  } }));
  await settled(); await settled();
  assert.ok(labelled(stage, '/usr/bin/healthy'));
  assert.ok(labelled(stage, '1 container process snapshot unavailable; available containers remain visible.'));
  assert.ok(labelled(stage, 'worker: container is stopped'));
  assert.equal(labelled(stage, 'Retry processes'), undefined, 'a partial snapshot remains usable rather than becoming a page-wide error');
});

test('large process inventories stay below the client pending-call window', async () => {
  let active = 0; let peak = 0; let completed = 0;
  const processApi = { containers: { processes: async () => {
    active += 1; peak = Math.max(peak, active);
    await settled();
    active -= 1; completed += 1;
    return { titles: ['PID'], processes: [], observed_at_ms: 1, scope: 'namespace', pid_identity: 'snapshot', truncated: false };
  } } };
  const stage = host();
  stage.render(h(Processes, { api: processApi, resource: {
    data: Array.from({ length: 25 }, (_, index) => ({ id: `container-${index}`, name: `container-${index}` })),
    loading: false, error: null, reload: async () => {},
  } }));
  while (completed < 25) await settled();
  assert.equal(peak, 8);
  assert.ok(labelled(stage, 'No running processes'));
});

test('a late process snapshot cannot replace a newer container inventory', async () => {
  const pending = new Map();
  const processApi = { containers: { processes: (id) => new Promise((resolve) => pending.set(id, resolve)) } };
  const stage = host();
  const resource = (id, name) => ({ data: [{ id, name }], loading: false, error: null, reload: async () => {} });
  stage.render(h(Processes, { api: processApi, resource: resource('old', 'former') }));
  await settled();
  stage.render(h(Processes, { api: processApi, resource: resource('new', 'current') }));
  await settled();
  pending.get('new')({ titles: ['PID', 'COMMAND'], processes: [['22', '/usr/bin/current']], observed_at_ms: 2, scope: 'namespace', pid_identity: 'snapshot', truncated: false });
  await settled(); await settled();
  assert.ok(labelled(stage, '/usr/bin/current'));
  pending.get('old')({ titles: ['PID', 'COMMAND'], processes: [['11', '/usr/bin/stale']], observed_at_ms: 1, scope: 'namespace', pid_identity: 'snapshot', truncated: false });
  await settled(); await settled();
  assert.ok(labelled(stage, '/usr/bin/current'));
  assert.equal(labelled(stage, '/usr/bin/stale'), undefined, 'superseded process authority never reaches the tree');
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
  stage.render(h(Top, { api: observed, initial: { containers: [], executions: [], images: [], volumes: [], networks: [] } }));
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

test('an image pull status older than its announced revision is ignored', async () => {
  const statuses = []; let publish; let reloads = 0;
  const controlled = { ...api, images: { ...api.images,
    startPull: async () => ({ job: 'ordered' }),
    pullStatus: async () => statuses.shift(),
    cancelPull: async () => {},
  }, watchImagePulls: async (listener) => { publish = listener; return async () => {}; } };
  const stage = host();
  stage.render(h(Images, { api: controlled, resource: { data: [], loading: false, error: null, reload: async () => { reloads += 1; } } }));
  change(stage, 'registry/image:tag', 'alpine:3.20'); invoke(stage, 'Pull'); await settled(); await settled();
  statuses.push({ job: 'ordered', reference: 'alpine:3.20', revision: 2, state: 'pulling', status: 'Stale read', layer: 'stale', current: 20, total: 100, image: null, error: null });
  await publish({ job: 'ordered', revision: 3, state: 'pulling', coalesced: 0 });
  await settled(); await settled();
  assert.equal(labelled(stage, 'Layer stale'), undefined, 'status older than its triggering event has no authority');
  statuses.push({ job: 'ordered', reference: 'alpine:3.20', revision: 4, state: 'complete', status: 'Pull complete', layer: null, current: 100, total: 100, image: { id: 'i1' }, error: null });
  await publish({ job: 'ordered', revision: 4, state: 'complete', coalesced: 0 });
  await settled(); await settled();
  assert.ok(labelled(stage, 'Pulled alpine:3.20.'));
  assert.equal(reloads, 1, 'only the accepted completion refreshes inventory');
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

test('container creation groups its compact form and explains raw JSON before an error', () => {
  const stage = host();
  const frame = stage.render(h(Containers, { api, resource: { data: [], loading: false, error: null, reload: async () => {} } }));
  for (const label of [
    'Identity and image', 'Process', 'Resources and connectivity',
    'Labels use JSON [name, value] pairs, for example [["role","worker"]].',
    'Entrypoint and command use JSON argv arrays; environment uses JSON [name, value] pairs.',
    'Mounts and ports use JSON object arrays; host filesystem paths and host addresses are not accepted.',
  ]) assert.ok(labelled(stage, label), `${label} is available in the semantic tree`);
  const placeholders = frame.patches.filter((patch) => patch.SetProp?.prop === 'Placeholder').map((patch) => patch.SetProp.value.Text);
  assert.deepEqual(placeholders.slice(0, 15), [
    'Image reference', 'Container name', 'Hostname (optional)', 'Run as user (optional)', 'Labels JSON (optional)',
    'Entrypoint argv JSON (optional)', 'Command argv JSON (optional)', 'Environment pairs JSON (optional)', 'Working directory (optional)',
    'Memory limit MiB (optional)', 'CPU limit (optional)', 'PID limit (optional)', 'Initial network (optional)',
    'Named volume mounts JSON (optional)', 'Published ports JSON (optional)',
  ], 'visual grouping preserves a predictable keyboard traversal order');
  const wrappingRows = frame.patches.filter((patch) => patch.SetProp?.prop === 'Wrap' && patch.SetProp.value?.Flag === true);
  assert.equal(wrappingRows.length >= 3, true, 'every field group can wrap at compact width');
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
  change(stage, 'Command argv JSON (optional)', '["sh",7]');
  assert.ok(labelled(stage, 'Command must contain at most 64 NUL-free string arguments, each at most 4096 bytes and 32768 bytes in total.'));
  assert.equal(isEnabled(stage, 'Create and start'), false, 'invalid optional configuration cannot reach the host');
  change(stage, 'Command argv JSON (optional)', '');
  change(stage, 'Environment pairs JSON (optional)', '[["MODE","one"],["MODE","two"]]');
  assert.ok(labelled(stage, 'Environment must contain at most 256 unique [name, value] pairs with bounded NUL-free strings.'));
  assert.equal(isEnabled(stage, 'Create and start'), false);
  change(stage, 'Environment pairs JSON (optional)', '');
  change(stage, 'Working directory (optional)', '/workspace/../secret');
  assert.ok(labelled(stage, 'Working directory must be an absolute, NUL-free path without dot segments and at most 4096 bytes.'));
  assert.equal(isEnabled(stage, 'Create and start'), false);
  change(stage, 'Working directory (optional)', '');
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

test('container creation validates exact resource bounds and retains them until success', async () => {
  const calls = [];
  let creates = 0;
  const controlled = { containers: {
    create: async (spec) => {
      calls.push(['create', spec]); creates += 1;
      if (creates === 1) throw new Error('create temporarily unavailable');
      return 'limited-container';
    },
    start: async (id) => calls.push(['start', id]),
  } };
  const resource = { data: [], loading: false, error: null, reload: async () => calls.push(['reload']) };
  const stage = host();
  stage.render(h(Containers, { api: controlled, resource }));
  change(stage, 'Image reference', 'alpine:3.20');
  change(stage, 'Container name', 'limited');
  for (const [placeholder, maximum, label] of [
    ['Memory limit MiB (optional)', 1_048_576, 'Memory limit'],
    ['CPU limit (optional)', 256, 'CPU limit'],
    ['PID limit (optional)', 1_000_000, 'PID limit'],
  ]) {
    change(stage, placeholder, '0');
    assert.ok(labelled(stage, `${label} must be a whole decimal number from 1 to ${maximum}.`));
    assert.equal(isEnabled(stage, 'Create and start'), false);
    change(stage, placeholder, String(maximum + 1));
    assert.ok(labelled(stage, `${label} must be a whole decimal number from 1 to ${maximum}.`));
    change(stage, placeholder, '1.5');
    assert.ok(labelled(stage, `${label} must be a whole decimal number from 1 to ${maximum}.`));
    change(stage, placeholder, String(maximum));
    assert.equal(isEnabled(stage, 'Create and start'), true, `${label} upper boundary is accepted`);
  }

  invoke(stage, 'Create and start'); await settled(); await settled();
  assert.ok(labelled(stage, 'create temporarily unavailable'));
  assert.equal(fieldValue(stage, 'Memory limit MiB (optional)'), '1048576');
  assert.equal(fieldValue(stage, 'CPU limit (optional)'), '256');
  assert.equal(fieldValue(stage, 'PID limit (optional)'), '1000000');
  invoke(stage, 'Create and start'); await settled(); await settled();
  assert.deepEqual(calls, [
    ['create', { image: 'alpine:3.20', name: 'limited', memory_mb: 1_048_576, cpus: 256, pids_limit: 1_000_000 }],
    ['create', { image: 'alpine:3.20', name: 'limited', memory_mb: 1_048_576, cpus: 256, pids_limit: 1_000_000 }],
    ['start', 'limited-container'], ['reload'],
  ]);
  assert.equal(fieldValue(stage, 'Memory limit MiB (optional)'), '');
  assert.equal(fieldValue(stage, 'CPU limit (optional)'), '');
  assert.equal(fieldValue(stage, 'PID limit (optional)'), '');
});

test('container creation accepts only bounded named-volume mounts and retains them until success', async () => {
  const calls = [];
  let creates = 0;
  const controlled = { containers: {
    create: async (spec) => {
      calls.push(['create', spec]); creates += 1;
      if (creates === 1) throw new Error('volume attachment temporarily unavailable');
      return 'mounted-container';
    },
    start: async (id) => calls.push(['start', id]),
  } };
  const resource = { data: [], loading: false, error: null, reload: async () => calls.push(['reload']) };
  const stage = host();
  stage.render(h(Containers, { api: controlled, resource }));
  change(stage, 'Image reference', 'alpine:3.20');
  change(stage, 'Container name', 'mounted');
  const placeholder = 'Named volume mounts JSON (optional)';
  const error = 'Mounts must contain at most 64 named volumes with unique absolute targets and optional boolean read_only. Host bind mounts are not accepted.';
  for (const invalid of [
    '[{"volume":"cache","target":"relative"}]',
    '[{"volume":"cache","target":"/cache/../secret"}]',
    '[{"volume":"cache","target":"/cache","read_only":"yes"}]',
    '[{"volume":"cache","target":"/same"},{"volume":"data","target":"/same"}]',
    JSON.stringify(Array.from({ length: 65 }, (_, index) => ({ volume: `v${index}`, target: `/v${index}` }))),
    '[{"source":"/host","target":"/guest"}]',
  ]) {
    change(stage, placeholder, invalid);
    assert.ok(labelled(stage, error));
    assert.equal(isEnabled(stage, 'Create and start'), false);
  }
  change(stage, placeholder, JSON.stringify(Array.from({ length: 64 }, (_, index) => ({ volume: `v${index}`, target: `/v${index}` }))));
  assert.equal(isEnabled(stage, 'Create and start'), true, 'the exact 64-mount boundary is accepted');
  const requested = '[{"volume":"cache","target":"/cache","read_only":true},{"volume":"data","target":"/srv/data"}]';
  change(stage, placeholder, requested);
  invoke(stage, 'Create and start'); await settled(); await settled();
  assert.ok(labelled(stage, 'volume attachment temporarily unavailable'));
  assert.equal(fieldValue(stage, placeholder), requested);
  invoke(stage, 'Create and start'); await settled(); await settled();
  const spec = { image: 'alpine:3.20', name: 'mounted', mounts: [
    { volume: 'cache', target: '/cache', read_only: true },
    { volume: 'data', target: '/srv/data', read_only: false },
  ] };
  assert.deepEqual(calls, [['create', spec], ['create', spec], ['start', 'mounted-container'], ['reload']]);
  assert.equal(fieldValue(stage, placeholder), '');
});

test('container creation validates bounded published ports and retains them until success', async () => {
  const calls = [];
  let creates = 0;
  const controlled = { containers: {
    create: async (spec) => {
      calls.push(['create', spec]); creates += 1;
      if (creates === 1) throw new Error('port publication temporarily unavailable');
      return 'published-container';
    },
    start: async (id) => calls.push(['start', id]),
  } };
  const resource = { data: [], loading: false, error: null, reload: async () => calls.push(['reload']) };
  const stage = host();
  stage.render(h(Containers, { api: controlled, resource }));
  change(stage, 'Image reference', 'alpine:3.20');
  change(stage, 'Container name', 'published');
  const placeholder = 'Published ports JSON (optional)';
  const error = 'Ports must contain at most 64 unique container-port/protocol pairs from 1 to 65535; host is an optional port number, not an address.';
  for (const invalid of [
    '[{"container":0,"protocol":"tcp"}]',
    '[{"container":65536,"protocol":"tcp"}]',
    '[{"container":80,"host":0,"protocol":"tcp"}]',
    '[{"container":80,"host":"127.0.0.1:8080","protocol":"tcp"}]',
    '[{"container":80,"protocol":"sctp"}]',
    '[{"container":80,"protocol":"tcp"},{"container":80,"host":8080,"protocol":"tcp"}]',
    '[{"container":80,"protocol":"tcp","address":"127.0.0.1"}]',
    JSON.stringify(Array.from({ length: 65 }, (_, index) => ({ container: index + 1, protocol: 'tcp' }))),
  ]) {
    change(stage, placeholder, invalid);
    assert.ok(labelled(stage, error));
    assert.equal(isEnabled(stage, 'Create and start'), false);
  }
  change(stage, placeholder, JSON.stringify(Array.from({ length: 64 }, (_, index) => ({ container: index + 1, protocol: 'tcp' }))));
  assert.equal(isEnabled(stage, 'Create and start'), true, 'the exact 64-port boundary is accepted');
  const requested = '[{"container":8080,"host":18080,"protocol":"tcp"},{"container":53,"protocol":"udp"},{"container":53,"protocol":"tcp"}]';
  change(stage, placeholder, requested);
  invoke(stage, 'Create and start'); await settled(); await settled();
  assert.ok(labelled(stage, 'port publication temporarily unavailable'));
  assert.equal(fieldValue(stage, placeholder), requested);
  invoke(stage, 'Create and start'); await settled(); await settled();
  const spec = { image: 'alpine:3.20', name: 'published', ports: [
    { container: 8080, host: 18080, protocol: 'tcp' },
    { container: 53, host: null, protocol: 'udp' },
    { container: 53, host: null, protocol: 'tcp' },
  ] };
  assert.deepEqual(calls, [['create', spec], ['create', spec], ['start', 'published-container'], ['reload']]);
  assert.equal(fieldValue(stage, placeholder), '');
});

test('container creation validates runtime identity and retains it until success', async () => {
  const calls = [];
  let creates = 0;
  const controlled = { containers: {
    create: async (spec) => {
      calls.push(['create', spec]); creates += 1;
      if (creates === 1) throw new Error('identity temporarily unavailable');
      return 'identity-container';
    },
    start: async (id) => calls.push(['start', id]),
  } };
  const resource = { data: [], loading: false, error: null, reload: async () => calls.push(['reload']) };
  const stage = host();
  stage.render(h(Containers, { api: controlled, resource }));
  change(stage, 'Image reference', 'alpine:3.20');
  change(stage, 'Container name', 'identity');
  const hostnameError = 'Hostname must start with an ASCII letter or digit, contain only ASCII letters, digits, dots, underscores or hyphens, and be at most 253 bytes.';
  for (const invalid of ['-worker', 'worker name', 'wørker', `a${'b'.repeat(253)}`]) {
    change(stage, 'Hostname (optional)', invalid);
    assert.ok(labelled(stage, hostnameError));
    assert.equal(isEnabled(stage, 'Create and start'), false);
  }
  change(stage, 'Hostname (optional)', `a${'b'.repeat(252)}`);
  assert.equal(isEnabled(stage, 'Create and start'), true, 'the exact 253-byte hostname boundary is accepted');
  change(stage, 'Hostname (optional)', 'build-worker_1.local');
  change(stage, 'Run as user (optional)', `u${'é'.repeat(128)}`);
  assert.ok(labelled(stage, 'Run as user must be a nonempty, NUL-free value of at most 256 bytes.'));
  assert.equal(isEnabled(stage, 'Create and start'), false, 'UTF-8 byte length, not character count, enforces the user bound');
  const exactUser = `u${'é'.repeat(127)}x`;
  change(stage, 'Run as user (optional)', exactUser);
  invoke(stage, 'Create and start'); await settled(); await settled();
  assert.ok(labelled(stage, 'identity temporarily unavailable'));
  assert.equal(fieldValue(stage, 'Hostname (optional)'), 'build-worker_1.local');
  assert.equal(fieldValue(stage, 'Run as user (optional)'), exactUser);
  invoke(stage, 'Create and start'); await settled(); await settled();
  const spec = { image: 'alpine:3.20', name: 'identity', hostname: 'build-worker_1.local', user: exactUser };
  assert.deepEqual(calls, [['create', spec], ['create', spec], ['start', 'identity-container'], ['reload']]);
  assert.equal(fieldValue(stage, 'Hostname (optional)'), '');
  assert.equal(fieldValue(stage, 'Run as user (optional)'), '');
});

test('container creation validates bounded labels and retains them until success', async () => {
  const calls = [];
  let creates = 0;
  const controlled = { containers: {
    create: async (spec) => {
      calls.push(['create', spec]); creates += 1;
      if (creates === 1) throw new Error('label persistence temporarily unavailable');
      return 'labelled-container';
    },
    start: async (id) => calls.push(['start', id]),
  } };
  const resource = { data: [], loading: false, error: null, reload: async () => calls.push(['reload']) };
  const stage = host();
  stage.render(h(Containers, { api: controlled, resource }));
  change(stage, 'Image reference', 'alpine:3.20');
  change(stage, 'Container name', 'labelled');
  const placeholder = 'Labels JSON (optional)';
  const error = 'Labels must contain at most 128 unique [name, value] pairs; names are nonempty and at most 256 bytes, values at most 4096 bytes, and both are NUL-free.';
  for (const invalid of [
    '{"role":"worker"}',
    '[["","worker"]]',
    '[["role","worker"],["role","other"]]',
    JSON.stringify([[`k${'é'.repeat(128)}`, 'value']]),
    JSON.stringify([['key', 'é'.repeat(2049)]]),
    JSON.stringify(Array.from({ length: 129 }, (_, index) => [`key-${index}`, 'value'])),
  ]) {
    change(stage, placeholder, invalid);
    assert.ok(labelled(stage, error));
    assert.equal(isEnabled(stage, 'Create and start'), false);
  }
  change(stage, placeholder, JSON.stringify(Array.from({ length: 128 }, (_, index) => [`key-${index}`, 'value'])));
  assert.equal(isEnabled(stage, 'Create and start'), true, 'the exact 128-label boundary is accepted');
  const requested = '[["role","worker"],["com.example/tier","backend"],["empty",""]]';
  change(stage, placeholder, requested);
  invoke(stage, 'Create and start'); await settled(); await settled();
  assert.ok(labelled(stage, 'label persistence temporarily unavailable'));
  assert.equal(fieldValue(stage, placeholder), requested);
  invoke(stage, 'Create and start'); await settled(); await settled();
  const spec = { image: 'alpine:3.20', name: 'labelled', labels: [['role', 'worker'], ['com.example/tier', 'backend'], ['empty', '']] };
  assert.deepEqual(calls, [['create', spec], ['create', spec], ['start', 'labelled-container'], ['reload']]);
  assert.equal(fieldValue(stage, placeholder), '');
});

test('container creation validates entrypoint argv and retains it until success', async () => {
  const calls = [];
  let creates = 0;
  const controlled = { containers: {
    create: async (spec) => {
      calls.push(['create', spec]); creates += 1;
      if (creates === 1) throw new Error('entrypoint temporarily unavailable');
      return 'entrypoint-container';
    },
    start: async (id) => calls.push(['start', id]),
  } };
  const resource = { data: [], loading: false, error: null, reload: async () => calls.push(['reload']) };
  const stage = host();
  stage.render(h(Containers, { api: controlled, resource }));
  change(stage, 'Image reference', 'alpine:3.20');
  change(stage, 'Container name', 'entrypoint');
  const placeholder = 'Entrypoint argv JSON (optional)';
  const error = 'Entrypoint must contain 1 to 64 NUL-free string arguments, each at most 4096 bytes and 32768 bytes in total.';
  for (const invalid of [
    '[]', '[""]', '[1]', JSON.stringify(['x'.repeat(4097)]),
    JSON.stringify(Array.from({ length: 65 }, () => 'x')),
  ]) {
    change(stage, placeholder, invalid);
    assert.ok(labelled(stage, error));
    assert.equal(isEnabled(stage, 'Create and start'), false);
  }
  change(stage, placeholder, JSON.stringify(Array.from({ length: 64 }, () => 'x')));
  assert.equal(isEnabled(stage, 'Create and start'), true, 'the exact 64-argument boundary is accepted');
  change(stage, placeholder, JSON.stringify(['x'.repeat(4096)]));
  assert.equal(isEnabled(stage, 'Create and start'), true, 'the exact per-argument byte boundary is accepted');
  change(stage, placeholder, JSON.stringify(Array.from({ length: 4 }, () => 'e'.repeat(4096))));
  change(stage, 'Command argv JSON (optional)', JSON.stringify(Array.from({ length: 5 }, () => 'c'.repeat(4096))));
  assert.ok(labelled(stage, 'Entrypoint and command together must contain at most 32768 bytes.'));
  change(stage, 'Command argv JSON (optional)', JSON.stringify(Array.from({ length: 4 }, () => 'c'.repeat(4096))));
  assert.equal(isEnabled(stage, 'Create and start'), true, 'the exact combined 32768-byte boundary is accepted');
  change(stage, placeholder, '["/bin/sh","-lc"]');
  change(stage, 'Command argv JSON (optional)', '["printf ready"]');
  invoke(stage, 'Create and start'); await settled(); await settled();
  assert.ok(labelled(stage, 'entrypoint temporarily unavailable'));
  assert.equal(fieldValue(stage, placeholder), '["/bin/sh","-lc"]');
  invoke(stage, 'Create and start'); await settled(); await settled();
  const spec = { image: 'alpine:3.20', name: 'entrypoint', entrypoint: ['/bin/sh', '-lc'], command: ['printf ready'] };
  assert.deepEqual(calls, [['create', spec], ['create', spec], ['start', 'entrypoint-container'], ['reload']]);
  assert.equal(fieldValue(stage, placeholder), '');
});

test('container creation validates an initial network reference and retains it until success', async () => {
  const calls = [];
  let creates = 0;
  const controlled = { containers: {
    create: async (spec) => {
      calls.push(['create', spec]); creates += 1;
      if (creates === 1) throw new Error('network attachment temporarily unavailable');
      return 'networked-container';
    },
    start: async (id) => calls.push(['start', id]),
  } };
  const resource = { data: [], loading: false, error: null, reload: async () => calls.push(['reload']) };
  const stage = host();
  stage.render(h(Containers, { api: controlled, resource }));
  change(stage, 'Image reference', 'alpine:3.20');
  change(stage, 'Container name', 'networked');
  const placeholder = 'Initial network (optional)';
  const error = 'Initial network must start with an ASCII letter or digit, contain only ASCII letters, digits, dots, underscores or hyphens, and be at most 255 bytes.';
  for (const invalid of ['-private', 'private network', 'prívate', `n${'x'.repeat(255)}`]) {
    change(stage, placeholder, invalid);
    assert.ok(labelled(stage, error));
    assert.equal(isEnabled(stage, 'Create and start'), false);
  }
  change(stage, placeholder, `n${'x'.repeat(254)}`);
  assert.equal(isEnabled(stage, 'Create and start'), true, 'the exact 255-byte boundary is accepted');
  change(stage, placeholder, 'private_backend.v1');
  invoke(stage, 'Create and start'); await settled(); await settled();
  assert.ok(labelled(stage, 'network attachment temporarily unavailable'));
  assert.equal(fieldValue(stage, placeholder), 'private_backend.v1');
  invoke(stage, 'Create and start'); await settled(); await settled();
  const spec = { image: 'alpine:3.20', name: 'networked', network: 'private_backend.v1' };
  assert.deepEqual(calls, [['create', spec], ['create', spec], ['start', 'networked-container'], ['reload']]);
  assert.equal(fieldValue(stage, placeholder), '');
});

test('container removal authority is available only for a stopped inventory record', () => {
  const id = 'c'.repeat(32);
  const api = { containers: {} };
  const stage = host();
  const inventory = (state) => ({ data: [{ id, name: 'worker', image: 'alpine', state }], loading: false, error: null, reload: async () => {} });
  stage.render(h(Containers, { api, resource: inventory('running') }));
  assert.equal(isEnabled(stage, 'Remove'), false, 'a running container cannot be removed');

  stage.render(h(Containers, { api, resource: inventory('stopped') }));
  assert.equal(isEnabled(stage, 'Remove'), true, 'the authoritative stopped state enables removal consent');
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

function property(stage, label, prop) {
  const node = labelled(stage, label)?.SetProp.id;
  return stage.frames.flatMap((frame) => frame.patches).filter((patch) =>
    'SetProp' in patch && patch.SetProp.id === node && patch.SetProp.prop === prop).at(-1)?.SetProp.value;
}

function latestPropertyForTag(stage, tag, prop) {
  const patches = stage.frames.flatMap((frame) => frame.patches);
  const nodes = new Set(patches.filter((patch) => patch.Create?.tag === tag).map((patch) => patch.Create.id));
  return patches.filter((patch) => patch.SetProp?.prop === prop && nodes.has(patch.SetProp.id)).at(-1)?.SetProp.value;
}

function stageFromFrame(frame) { return { frames: [frame] }; }

function fieldValue(stage, placeholder) {
  const node = stage.frames.flatMap((frame) => frame.patches).filter((patch) =>
    'SetProp' in patch && patch.SetProp.prop === 'Placeholder' && patch.SetProp.value?.Text === placeholder).at(-1)?.SetProp.id;
  return stage.frames.flatMap((frame) => frame.patches).filter((patch) =>
    'SetProp' in patch && patch.SetProp.id === node && patch.SetProp.prop === 'Value').at(-1)?.SetProp.value?.Text;
}

const settled = () => new Promise((resolve) => setImmediate(resolve));

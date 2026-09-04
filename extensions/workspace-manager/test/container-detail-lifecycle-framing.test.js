import assert from 'node:assert/strict';
import { mkdtemp, rm } from 'node:fs/promises';
import net from 'node:net';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import { createElement as h } from 'react';
import { connect, workspace } from '../../../packages/react/src/index.js';
import { KIND, Reader, encode } from '../../../packages/react/src/wire.js';
import { WorkspaceManager } from '../src/app.js';
import { CONTAINER_DETAIL_SOURCE, ContainerDetailsSource } from '../src/model.js';
import { host } from './host.js';

test('authoritative container inventory replacement invalidates prior detail lifecycle', { timeout: 5_000 }, async () => {
  const directory = await mkdtemp(join(tmpdir(), 'husklet-container-detail-lifecycle-'));
  const socketPath = join(directory, 'host.sock');
  const id = 'c'.repeat(32);
  let listAttempts = 0;
  let inspectAttempts = 0;
  const server = net.createServer((socket) => {
    const reader = new Reader();
    socket.write(encode({ channel: 0, kind: KIND.open, payload: {
      protocol: 1, extension: 'container-detail-lifecycle-test', granted: ['container-read'],
    } }));
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        const call = frame.payload?.call;
        if (!call) continue;
        let payload = { reply: 'done' };
        if (call === 'container_list') {
          listAttempts += 1;
          payload = { reply: 'containers', with: [{
            id, name: listAttempts === 1 ? 'before-refresh' : 'after-refresh',
            image: listAttempts === 1 ? 'old:image' : 'new:image', state: 'running', created: listAttempts,
          }] };
        } else if (call === 'container_inspect') {
          inspectAttempts += 1;
          payload = { reply: 'container', with: {
            id, name: inspectAttempts === 1 ? 'before-refresh' : 'after-refresh',
            image: inspectAttempts === 1 ? 'old:image' : 'new:image', state: 'running', created: inspectAttempts,
          } };
        }
        const response = encode({ channel: frame.channel, kind: KIND.response, flags: 1, payload });
        setTimeout(() => socket.write(response), 20);
      }
    });
  });
  await new Promise((resolve, reject) => server.listen(socketPath, (error) => error ? reject(error) : resolve()));

  let session;
  let stage;
  try {
    session = await connect({ path: socketPath });
    const framed = workspace(session);
    const mutations = [];
    const details = new ContainerDetailsSource(async (mutation) => mutations.push(mutation));
    stage = host();
    stage.render(h(WorkspaceManager, {
      api: { ...framed, subscribe: undefined, unsubscribe: undefined }, containerDetails: details,
      initial: { executions: [], images: [], volumes: [], networks: [] },
    }));
    invoke(stage, 'Containers');
    await until(() => labelled(stage, 'before-refresh'));
    invoke(stage, 'Details');
    await until(() => lengths(mutations).length === 1 && labelled(stage, 'Hide details'));
    assert.deepEqual(lengths(mutations)[0], { source: CONTAINER_DETAIL_SOURCE, version: 1, rows: 5 });

    const refreshStart = stage.frames.length;
    invoke(stage, 'Refresh');
    await until(() => labelled(stage, 'after-refresh'));
    const refreshPatches = stage.frames.slice(refreshStart).flatMap((frame) => frame.patches);
    assert.ok(refreshPatches.some((patch) => 'Remove' in patch), 'refresh unmounts old detail');
    assert.ok(refreshPatches.some((patch) => patch.SetProp?.value?.Text === 'Details'), 'replacement returns closed');
    assert.equal(refreshPatches.some((patch) => patch.SetProp?.value?.Text === 'Hide details'), false,
      'old detail cannot remount for a reused immutable ID');
    assert.equal(lengths(mutations).length, 1, 'inventory replacement does not republish old detail');

    invoke(stage, 'Details');
    await until(() => lengths(mutations).length === 2);
    assert.deepEqual(lengths(mutations)[1], { source: CONTAINER_DETAIL_SOURCE, version: 2, rows: 5 });
    assert.equal(inspectAttempts, 2, 'current detail requires a new framed inspection');
  } finally {
    stage?.render(null);
    await new Promise((resolve) => setTimeout(resolve, 30));
    session?.close();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

function lengths(mutations) {
  return mutations.flatMap((mutation) => mutation.Length ? [mutation.Length] : []);
}

function labelled(stage, label) {
  return stage.frames.flatMap((frame) => frame.patches).filter((patch) =>
    patch.SetProp?.prop === 'Label' && patch.SetProp.value?.Text === label).at(-1);
}

function invoke(stage, label) {
  const nodes = stage.frames.flatMap((frame) => frame.patches).filter((patch) =>
    patch.SetProp?.prop === 'Label' && patch.SetProp.value?.Text === label)
    .map((patch) => patch.SetProp.id).reverse();
  assert.ok(nodes.some((node) => stage.surface.dispatch({ trigger: 'Invoke', node, id: `${node}:Invoke`, value: null })), `${label} invokes`);
}

async function until(done) {
  const deadline = Date.now() + 2_000;
  while (!done()) {
    if (Date.now() >= deadline) throw new Error('container detail lifecycle did not settle');
    await new Promise((resolve) => setTimeout(resolve, 10));
  }
}

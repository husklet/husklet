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
import { IMAGE_DETAIL_SOURCE, ImageDetailsSource } from '../src/model.js';
import { host } from './host.js';

test('authoritative image replacement invalidates detail and removal consent', { timeout: 5_000 }, async () => {
  const directory = await mkdtemp(join(tmpdir(), 'husklet-image-detail-lifecycle-'));
  const socketPath = join(directory, 'host.sock');
  const digest = `sha256:${'a'.repeat(64)}`;
  let listAttempts = 0;
  let inspectAttempts = 0;
  const server = net.createServer((socket) => {
    const reader = new Reader();
    socket.write(encode({ channel: 0, kind: KIND.open, payload: {
      protocol: 1, extension: 'image-detail-lifecycle-test', granted: ['image-read', 'image-write'],
    } }));
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        const call = frame.payload?.call;
        if (!call) continue;
        let payload = { reply: 'done' };
        if (call === 'image_list') {
          listAttempts += 1;
          payload = { reply: 'images', with: [{
            id: digest, reference: listAttempts === 1 ? 'old/image:1' : 'new/image:2', size: 1024, created: listAttempts,
          }] };
        } else if (call === 'image_inspect') {
          inspectAttempts += 1;
          payload = { reply: 'image_details', with: {
            id: digest, references: [inspectAttempts === 1 ? 'old/image:1' : 'new/image:2'],
            created: 'now', size: 1024, os: 'linux', architecture: 'amd64', entrypoint: [], command: [],
            working_directory: inspectAttempts === 1 ? '/old-work' : '/new-work', user: '',
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
    const details = new ImageDetailsSource(async (mutation) => mutations.push(mutation));
    stage = host();
    stage.render(h(WorkspaceManager, {
      api: { ...framed, subscribe: undefined, unsubscribe: undefined, watchImagePulls: undefined }, imageDetails: details,
      initial: { containers: [], executions: [], volumes: [], networks: [] },
    }));
    invoke(stage, 'Images');
    await until(() => labelled(stage, 'old/image:1'));
    invoke(stage, 'Inspect');
    await until(() => lengths(mutations).length === 1);
    assert.deepEqual(lengths(mutations)[0], { source: IMAGE_DETAIL_SOURCE, version: 1, rows: 8 });
    invoke(stage, 'Remove');
    assert.ok(labelled(stage, `Remove immutable image ${digest}?`));

    const refreshStart = stage.frames.length;
    invoke(stage, 'Refresh');
    await until(() => labelled(stage, 'new/image:2'));
    const refreshPatches = stage.frames.slice(refreshStart).flatMap((frame) => frame.patches);
    assert.ok(refreshPatches.some((patch) => 'Remove' in patch), 'refresh unmounts prior image detail and consent');
    assert.equal(refreshPatches.some((patch) => patch.SetProp?.value?.Text === `Remove immutable image ${digest}?`), false,
      'removal consent does not remount for a reused digest');
    assert.equal(refreshPatches.some((patch) => patch.SetProp?.value?.Text === '/old-work'), false,
      'old inspection does not remount for a reused digest');
    assert.equal(lengths(mutations).length, 1, 'inventory replacement cannot republish prior detail');

    invoke(stage, 'Inspect');
    await until(() => lengths(mutations).length === 2);
    assert.deepEqual(lengths(mutations)[1], { source: IMAGE_DETAIL_SOURCE, version: 2, rows: 8 });
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
    if (Date.now() >= deadline) throw new Error('image detail lifecycle did not settle');
    await new Promise((resolve) => setTimeout(resolve, 10));
  }
}

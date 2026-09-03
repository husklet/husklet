import assert from 'node:assert/strict';
import { mkdtemp, rm } from 'node:fs/promises';
import net from 'node:net';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import { createElement as h } from 'react';
import { connect, workspace } from '../../react/src/index.js';
import { KIND, Reader, encode } from '../../react/src/wire.js';
import { WorkspaceManager } from '../src/app.js';
import { host } from './host.js';

test('image inventory removes stale destructive controls across real framed refresh states', { timeout: 5_000 }, async () => {
  const directory = await mkdtemp(join(tmpdir(), 'husklet-image-list-resource-'));
  const socketPath = join(directory, 'host.sock');
  const digest = `sha256:${'a'.repeat(64)}`;
  let attempts = 0;
  const server = net.createServer((socket) => {
    const reader = new Reader();
    socket.write(encode({ channel: 0, kind: KIND.request, payload: {
      protocol: 1, extension: 'image-list-resource-test', granted: ['image-read', 'image-write'],
    } }));
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.payload?.call !== 'image_list') continue;
        attempts += 1;
        let flags = 1;
        let payload;
        if (attempts === 2) {
          flags = 3;
          payload = { error: 'failed', detail: 'image inventory unavailable' };
        } else {
          payload = { reply: 'images', with: attempts === 3 ? [] : [{
            id: digest, reference: attempts === 1 ? 'stale/image:1' : 'current/image:2', size: 1024, created: 0,
          }] };
        }
        const response = encode({ channel: frame.channel, kind: KIND.response, flags, payload });
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
    stage = host();
    stage.render(h(WorkspaceManager, {
      api: { ...framed, subscribe: undefined, unsubscribe: undefined, watchImagePulls: undefined },
      initial: { containers: [], executions: [], volumes: [], networks: [] },
    }));
    invoke(stage, 'Images');
    await until(() => labelled(stage, 'Reading images…'));
    await until(() => labelled(stage, 'stale/image:1'));
    assert.ok(labelled(stage, 'Remove'));
    assert.ok(labelled(stage, 'Prune unused images'));

    const refreshStart = stage.frames.length;
    invoke(stage, 'Refresh');
    await until(() => attempts === 2 && labelled(stage, 'Reading images…'));
    const refreshPatches = stage.frames.slice(refreshStart).flatMap((frame) => frame.patches);
    assert.ok(refreshPatches.some((patch) => 'Remove' in patch), 'loading unmounts stale image cards and prune authority');
    await until(() => labelled(stage, 'image inventory unavailable'));
    for (const stale of ['Remove', 'Prune unused images']) {
      assert.equal(refreshPatches.some((patch) => patch.SetProp?.value?.Text === stale), false, `${stale} is not recreated during failure`);
    }

    invoke(stage, 'Retry images');
    await until(() => labelled(stage, 'No images'));
    assert.equal(attempts, 3);

    invoke(stage, 'Refresh');
    await until(() => labelled(stage, 'current/image:2'));
    assert.equal(attempts, 4);
    assert.ok(labelled(stage, 'Remove'));
    assert.ok(labelled(stage, 'Prune unused images'));
  } finally {
    stage?.render(null);
    await new Promise((resolve) => setTimeout(resolve, 30));
    session?.close();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

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
    if (Date.now() >= deadline) throw new Error('image list resource state did not settle');
    await new Promise((resolve) => setTimeout(resolve, 10));
  }
}

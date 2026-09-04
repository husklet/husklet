import assert from 'node:assert/strict';
import { mkdtemp, rm } from 'node:fs/promises';
import net from 'node:net';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import { createElement as h } from 'react';
import { connect, workspace } from '../../../packages/react/src/index.js';
import { KIND, Reader, encode } from '../../../packages/react/src/wire.js';
import { WorkspaceManager } from '../dist/app.js';
import { host } from './host.js';

test('volume inventory removes stale generation authority across real framed refresh states', { timeout: 5_000 }, async () => {
  const directory = await mkdtemp(join(tmpdir(), 'husklet-volume-list-resource-'));
  const socketPath = join(directory, 'host.sock');
  let attempts = 0;
  const server = net.createServer((socket) => {
    const reader = new Reader();
    socket.write(encode({ channel: 0, kind: KIND.open, payload: {
      protocol: 1, extension: 'volume-list-resource-test', granted: ['volume-read', 'volume-write'],
    } }));
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.payload?.call !== 'volume_list') continue;
        attempts += 1;
        let flags = 1;
        let payload;
        if (attempts === 2) {
          flags = 3;
          payload = { error: 'failed', detail: 'volume inventory unavailable' };
        } else {
          payload = { reply: 'volumes', with: attempts === 3 ? [] : [{
            name: attempts === 1 ? 'stale-cache' : 'current-cache', driver: 'local',
            generation: (attempts === 1 ? 'a' : 'b').repeat(32),
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
      api: { ...framed, subscribe: undefined, unsubscribe: undefined },
      initial: { containers: [], executions: [], images: [], networks: [] },
    }));
    invoke(stage, 'Volumes');
    await until(() => labelled(stage, 'Reading volumes…'));
    await until(() => labelled(stage, 'stale-cache'));
    invoke(stage, 'Remove');
    assert.ok(labelled(stage, `Remove volume stale-cache generation ${'a'.repeat(32)}?`));

    const refreshStart = stage.frames.length;
    invoke(stage, 'Refresh');
    await until(() => attempts === 2 && labelled(stage, 'Reading volumes…'));
    const refreshPatches = stage.frames.slice(refreshStart).flatMap((frame) => frame.patches);
    assert.ok(refreshPatches.some((patch) => 'Remove' in patch), 'loading unmounts stale volume card and confirmation');
    await until(() => labelled(stage, 'volume inventory unavailable'));
    for (const stale of ['Inspect', 'Remove', 'Confirm remove']) {
      assert.equal(refreshPatches.some((patch) => patch.SetProp?.value?.Text === stale), false, `${stale} is not recreated during failure`);
    }

    invoke(stage, 'Retry volumes');
    await until(() => labelled(stage, 'No volumes'));
    assert.equal(attempts, 3);

    invoke(stage, 'Refresh');
    await until(() => labelled(stage, 'current-cache'));
    assert.equal(attempts, 4);
    assert.ok(labelled(stage, 'Remove'));
    assert.ok(labelled(stage, 'Inspect'));
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
    if (Date.now() >= deadline) throw new Error('volume list resource state did not settle');
    await new Promise((resolve) => setTimeout(resolve, 10));
  }
}

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
import { host } from './host.js';

test('network inventory removes stale identity authority across real framed refresh states', { timeout: 5_000 }, async () => {
  const directory = await mkdtemp(join(tmpdir(), 'husklet-network-list-resource-'));
  const socketPath = join(directory, 'host.sock');
  const staleId = 'a'.repeat(32);
  const currentId = 'b'.repeat(32);
  const containerId = 'c'.repeat(32);
  let attempts = 0;
  const server = net.createServer((socket) => {
    const reader = new Reader();
    socket.write(encode({ channel: 0, kind: KIND.open, payload: {
      protocol: 1, extension: 'network-list-resource-test', granted: ['network-read', 'network-write'],
    } }));
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.payload?.call !== 'network_list') continue;
        attempts += 1;
        let flags = 1;
        let payload;
        if (attempts === 2) {
          flags = 3;
          payload = { error: 'failed', detail: 'network inventory unavailable' };
        } else {
          payload = { reply: 'networks', with: attempts === 3 ? [] : [{
            id: attempts === 1 ? staleId : currentId,
            name: attempts === 1 ? 'stale-net' : 'current-net', driver: 'bridge', scope: 'local',
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
      initial: { containers: [], executions: [], images: [], volumes: [] },
    }));
    invoke(stage, 'Networks');
    await until(() => labelled(stage, 'Reading networks…'));
    await until(() => labelled(stage, 'stale-net'));
    change(stage, 'Complete container ID', containerId);
    invoke(stage, 'Disconnect');
    assert.ok(labelled(stage, `Disconnect immutable container ${containerId} from network ${staleId}?`));
    invoke(stage, 'Remove');
    assert.ok(labelled(stage, `Remove immutable network ${staleId} (stale-net)?`));

    const refreshStart = stage.frames.length;
    invoke(stage, 'Refresh');
    await until(() => attempts === 2 && labelled(stage, 'Reading networks…'));
    const refreshPatches = stage.frames.slice(refreshStart).flatMap((frame) => frame.patches);
    assert.ok(refreshPatches.some((patch) => 'Remove' in patch), 'loading unmounts stale network controls and confirmations');
    await until(() => labelled(stage, 'network inventory unavailable'));
    for (const stale of ['Inspect', 'Connect', 'Disconnect', 'Remove', 'Confirm disconnect', 'Confirm remove']) {
      assert.equal(refreshPatches.some((patch) => patch.SetProp?.value?.Text === stale), false, `${stale} is not recreated during failure`);
    }

    invoke(stage, 'Retry networks');
    await until(() => labelled(stage, 'No networks'));
    assert.equal(attempts, 3);

    invoke(stage, 'Refresh');
    await until(() => labelled(stage, 'current-net'));
    assert.equal(attempts, 4);
    for (const control of ['Inspect', 'Connect', 'Disconnect', 'Remove']) assert.ok(labelled(stage, control));
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

function change(stage, placeholder, value) {
  const node = stage.frames.flatMap((frame) => frame.patches).filter((patch) =>
    patch.SetProp?.prop === 'Placeholder' && patch.SetProp.value?.Text === placeholder).at(-1)?.SetProp.id;
  assert.ok(stage.surface.dispatch({ trigger: 'Change', node, id: `${node}:Change`, value }), `${placeholder} changes`);
}

async function until(done) {
  const deadline = Date.now() + 2_000;
  while (!done()) {
    if (Date.now() >= deadline) throw new Error('network list resource state did not settle');
    await new Promise((resolve) => setTimeout(resolve, 10));
  }
}

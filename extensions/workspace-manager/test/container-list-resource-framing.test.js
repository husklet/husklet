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

test('container catalogue removes stale authority across framed loading, failure, retry and empty states', { timeout: 5_000 }, async () => {
  const directory = await mkdtemp(join(tmpdir(), 'husklet-container-list-resource-'));
  const socketPath = join(directory, 'host.sock');
  const id = 'c'.repeat(32);
  let attempts = 0;
  let removed = false;
  const removals = [];
  const server = net.createServer((socket) => {
    const reader = new Reader();
    socket.write(encode({ channel: 0, kind: KIND.open, payload: {
      protocol: 1, extension: 'container-list-resource-test', granted: ['container-read', 'container-control'],
    } }));
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        const call = frame.payload?.call;
        if (!call) continue;
        let flags = 1;
        let payload = { reply: 'done' };
        if (call === 'container_list') {
          attempts += 1;
          if (attempts === 2) {
            flags = 3;
            payload = { error: 'failed', detail: 'container inventory unavailable' };
          } else {
            payload = { reply: 'containers', with: attempts === 3 || removed ? [] : [
              { id, name: attempts === 1 ? 'stale-worker' : 'current-worker', image: 'alpine:3.20', state: 'stopped', created: 0 },
            ] };
          }
        } else if (call === 'container_remove') {
          removals.push(frame.payload.with);
          removed = true;
        }
        const response = encode({ channel: frame.channel, kind: KIND.response, flags, payload });
        setTimeout(() => socket.write(response), call === 'container_list' ? 20 : 0);
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
      initial: { executions: [], images: [], volumes: [], networks: [] },
    }));
    invoke(stage, 'Containers');
    await until(() => labelled(stage, 'Reading containers…'));
    await until(() => labelled(stage, 'stale-worker'));
    assert.ok(labelled(stage, 'Start'));
    invoke(stage, 'Remove');
    assert.ok(labelled(stage, `Remove stopped container stale-worker with immutable ID ${id}?`));

    const refreshStart = stage.frames.length;
    invoke(stage, 'Refresh');
    await until(() => attempts === 2 && labelled(stage, 'Reading containers…'));
    assert.ok(stage.frames.slice(refreshStart).flatMap((frame) => frame.patches).some((patch) => 'Remove' in patch),
      'refresh loading unmounts the prior container cards');
    await until(() => labelled(stage, 'container inventory unavailable'));
    assert.equal(stage.frames.slice(refreshStart).flatMap((frame) => frame.patches).some((patch) =>
      patch.SetProp?.prop === 'Label' && patch.SetProp.value?.Text === 'Start'), false,
    'failed refresh does not render replacement stale lifecycle authority');
    assert.equal(stage.frames.slice(refreshStart).flatMap((frame) => frame.patches).some((patch) =>
      patch.SetProp?.prop === 'Label' && patch.SetProp.value?.Text === 'Confirm remove'), false,
    'failed refresh cannot retain stale removal consent');

    invoke(stage, 'Retry containers');
    await until(() => labelled(stage, 'No containers'));
    assert.equal(attempts, 3);

    invoke(stage, 'Refresh');
    await until(() => labelled(stage, 'current-worker'));
    assert.equal(attempts, 4);
    assert.ok(labelled(stage, 'Start'), 'ready state restores lifecycle controls for current inventory');
    invoke(stage, 'Remove');
    assert.ok(labelled(stage, `Remove stopped container current-worker with immutable ID ${id}?`));
    invoke(stage, 'Confirm remove');
    await until(() => removals.length === 1 && labelled(stage, 'No containers'));
    assert.deepEqual(removals, [{ id }], 'removal uses the exact immutable inventory identity');
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
    if (Date.now() >= deadline) throw new Error('container list resource state did not settle');
    await new Promise((resolve) => setTimeout(resolve, 10));
  }
}

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

test('execution catalogue removes stale actions across framed loading, failure, retry and empty states', { timeout: 5_000 }, async () => {
  const directory = await mkdtemp(join(tmpdir(), 'husklet-execution-list-resource-'));
  const socketPath = join(directory, 'host.sock');
  const id = 'e'.repeat(32);
  const container = 'c'.repeat(32);
  let attempts = 0;
  const server = net.createServer((socket) => {
    const reader = new Reader();
    socket.write(encode({ channel: 0, kind: KIND.request, payload: {
      protocol: 1, extension: 'execution-list-resource-test', granted: ['container-read', 'container-control'],
    } }));
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        const call = frame.payload?.call;
        if (!call) continue;
        let flags = 1;
        let payload = { reply: 'done' };
        if (call === 'execution_list') {
          attempts += 1;
          if (attempts === 2) {
            flags = 3;
            payload = { error: 'failed', detail: 'execution inventory unavailable' };
          } else {
            payload = { reply: 'executions', with: { executions: attempts === 3 ? [] : [{
              id, container_id: container, running: true, exit_code: 0, pid: 42,
              command: [attempts === 1 ? 'stale-command' : 'current-command'], user: 'root',
            }], truncated: attempts === 1 } };
          }
        }
        const response = encode({ channel: frame.channel, kind: KIND.response, flags, payload });
        setTimeout(() => socket.write(response), call === 'execution_list' ? 20 : 0);
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
      api: { ...framed, subscribe: undefined, unsubscribe: undefined, watchExecutions: undefined },
      initial: { containers: [], images: [], volumes: [], networks: [] },
    }));
    invoke(stage, 'Executions');
    await until(() => labelled(stage, 'Reading executions…'));
    await until(() => labelled(stage, 'stale-command'));
    assert.ok(labelled(stage, 'Terminate'));
    assert.ok(labelled(stage, 'The host execution catalogue was truncated at its safety limit.'));

    const refreshStart = stage.frames.length;
    invoke(stage, 'Refresh');
    await until(() => attempts === 2 && labelled(stage, 'Reading executions…'));
    const refreshPatches = stage.frames.slice(refreshStart).flatMap((frame) => frame.patches);
    assert.ok(refreshPatches.some((patch) => 'Remove' in patch), 'loading unmounts stale execution cards');
    await until(() => labelled(stage, 'execution inventory unavailable'));
    assert.equal(refreshPatches.some((patch) => patch.SetProp?.value?.Text === 'Terminate'), false);
    assert.equal(refreshPatches.some((patch) => patch.SetProp?.value?.Text === 'The host execution catalogue was truncated at its safety limit.'), false);

    invoke(stage, 'Retry executions');
    await until(() => labelled(stage, 'No executions'));
    assert.equal(attempts, 3);

    invoke(stage, 'Refresh');
    await until(() => labelled(stage, 'current-command'));
    assert.equal(attempts, 4);
    assert.ok(labelled(stage, 'Terminate'), 'ready state restores actions for current execution inventory');
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
    if (Date.now() >= deadline) throw new Error('execution list resource state did not settle');
    await new Promise((resolve) => setTimeout(resolve, 10));
  }
}

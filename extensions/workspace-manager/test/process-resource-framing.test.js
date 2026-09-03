import assert from 'node:assert/strict';
import { mkdtemp, rm } from 'node:fs/promises';
import net from 'node:net';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import { createElement as h } from 'react';
import { connect, workspace } from '../../react/src/index.js';
import { KIND, Reader, encode } from '../../react/src/wire.js';
import { Processes } from '../src/app.js';
import { host } from './host.js';

test('process snapshots remove stale PID and scope claims across framed refresh states', { timeout: 5_000 }, async () => {
  const directory = await mkdtemp(join(tmpdir(), 'husklet-process-resource-'));
  const socketPath = join(directory, 'host.sock');
  const container = 'c'.repeat(32);
  let attempts = 0;
  const server = net.createServer((socket) => {
    const reader = new Reader();
    socket.write(encode({ channel: 0, kind: KIND.request, payload: {
      protocol: 1, extension: 'process-resource-test', granted: ['container-read'],
    } }));
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.payload?.call !== 'container_processes') continue;
        attempts += 1;
        let flags = 1;
        let payload;
        if (attempts === 2) {
          flags = 3;
          payload = { error: 'failed', detail: 'process snapshot unavailable' };
        } else {
          payload = { reply: 'processes', with: {
            titles: ['PID', 'COMMAND'],
            processes: attempts === 3 ? [] : [[attempts === 1 ? '41' : '84', attempts === 1 ? 'stale-process' : 'current-process']],
            observed_at_ms: attempts === 1 ? 1_700_000_000_000 : 1_700_000_001_000,
            scope: attempts === 4 ? 'namespace' : 'initial', pid_identity: 'snapshot', truncated: attempts === 1,
          } };
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
    stage = host();
    stage.render(h(Processes, {
      api: workspace(session), resource: { data: [{ id: container, name: 'worker' }], loading: false, error: null, reload: async () => {} },
    }));
    await until(() => labelled(stage, 'Reading processes…'));
    await until(() => labelled(stage, 'stale-process'));
    assert.ok(labelled(stage, 'PID 41'));
    assert.ok(labelled(stage, 'Initial processes only; PIDs identify this snapshot and may be reused.'));
    assert.ok(labelled(stage, 'The host process snapshot was truncated at its safety limit.'));

    const refreshStart = stage.frames.length;
    invoke(stage, 'Refresh');
    await until(() => attempts === 2 && labelled(stage, 'Reading processes…'));
    const refreshPatches = stage.frames.slice(refreshStart).flatMap((frame) => frame.patches);
    assert.ok(refreshPatches.some((patch) => 'Remove' in patch), 'loading unmounts stale PID rows and claims');
    await until(() => labelled(stage, 'process snapshot unavailable'));
    for (const stale of ['PID 41', 'Initial processes only; PIDs identify this snapshot and may be reused.', 'The host process snapshot was truncated at its safety limit.']) {
      assert.equal(refreshPatches.some((patch) => patch.SetProp?.value?.Text === stale), false, `${stale} is not recreated during failure`);
    }

    invoke(stage, 'Retry processes');
    await until(() => labelled(stage, 'No running processes'));
    assert.equal(attempts, 3);

    invoke(stage, 'Refresh');
    await until(() => labelled(stage, 'current-process'));
    assert.ok(labelled(stage, 'PID 84'));
    assert.ok(labelled(stage, 'Full container namespace snapshots; PIDs identify only this observation and may be reused.'));
    assert.equal(attempts, 4);
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
    if (Date.now() >= deadline) throw new Error('process resource state did not settle');
    await new Promise((resolve) => setTimeout(resolve, 10));
  }
}

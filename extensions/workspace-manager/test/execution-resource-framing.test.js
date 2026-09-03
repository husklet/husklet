import assert from 'node:assert/strict';
import { mkdtemp, rm } from 'node:fs/promises';
import net from 'node:net';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import { createElement as h } from 'react';
import { connect, workspace } from '../../react/src/index.js';
import { KIND, Reader, encode } from '../../react/src/wire.js';
import { Executions } from '../src/app.js';
import { EXECUTION_DETAIL_SOURCE, ExecutionDetailsSource } from '../src/model.js';
import { host } from './host.js';

test('execution detail traverses every ResourceState over real framing and drops stale detail', { timeout: 5_000 }, async () => {
  const directory = await mkdtemp(join(tmpdir(), 'husklet-execution-resource-'));
  const socketPath = join(directory, 'host.sock');
  const executionId = 'e'.repeat(32);
  const containerId = 'c'.repeat(32);
  const requests = [];
  let inspectAttempts = 0;
  const server = net.createServer((socket) => {
    const reader = new Reader();
    socket.write(encode({ channel: 0, kind: KIND.request, payload: {
      protocol: 1, extension: 'execution-resource-test', granted: ['container-read', 'container-control'],
    } }));
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (!frame.payload?.call) continue;
        requests.push(frame.payload);
        let payload = { reply: 'done' };
        let flags = 1;
        if (frame.payload.call === 'execution_inspect') {
          inspectAttempts += 1;
          if (inspectAttempts === 2) {
            flags = 3;
            payload = { error: 'failed', detail: 'execution inspect unavailable' };
          } else if (inspectAttempts === 3) {
            payload = { reply: 'execution', with: {} };
          } else {
            payload = { reply: 'execution', with: {
              id: executionId, container_id: containerId, running: true, exit_code: 0,
              pid: 42, command: ['sh', '-lc', 'printf ready'], user: '1000:1000',
            } };
          }
        }
        const response = encode({ channel: frame.channel, kind: KIND.response, flags, payload });
        setTimeout(() => socket.write(response), frame.payload.call === 'execution_inspect' ? 20 : 0);
      }
    });
  });

  await new Promise((resolve, reject) => server.listen(socketPath, (error) => error ? reject(error) : resolve()));
  let session;
  try {
    session = await connect({ path: socketPath });
    const api = workspace(session);
    const mutations = [];
    const source = new ExecutionDetailsSource(async (mutation) => mutations.push(mutation));
    const resource = { data: [{
      id: executionId, container_id: containerId, running: true, exit_code: 0,
      pid: 42, command: ['sh', '-lc', 'printf ready'], user: '1000:1000',
    }], loading: false, error: null, reload: async () => {} };
    const stage = host();
    stage.render(h(Executions, { api, resource, executionDetails: source }));

    for (const control of ['Load output', 'Wait up to 5s', 'Terminate', 'Remove record']) {
      assert.ok(labelled(stage, control), `${control} remains available`);
    }
    invoke(stage, 'Details');
    await until(() => labelled(stage, 'Reading execution details…'));
    await until(() => lengths(mutations).length === 1);
    assert.equal(lengths(mutations)[0].rows, 6);

    invoke(stage, 'Terminate');
    const refreshStart = stage.frames.length;
    invoke(stage, 'Confirm SIGTERM');
    await until(() => requests.some((request) => request.call === 'execution_kill'));
    await until(() => labelled(stage, 'Reading execution details…') && inspectAttempts === 2);
    const refreshPatches = stage.frames.slice(refreshStart).flatMap((frame) => frame.patches);
    assert.ok(refreshPatches.some((patch) => 'Remove' in patch), 'loading unmounts the prior detail table');
    await until(() => labelled(stage, 'execution inspect unavailable'));
    assert.equal(lengths(mutations).length, 1, 'failed refresh cannot republish stale detail rows');

    invoke(stage, 'Retry details');
    await until(() => labelled(stage, 'No execution details'));
    assert.deepEqual(lengths(mutations).at(-1), { source: EXECUTION_DETAIL_SOURCE, version: 2, rows: 0 });

    invoke(stage, 'Hide details');
    invoke(stage, 'Details');
    await until(() => lengths(mutations).length === 3);
    assert.deepEqual(lengths(mutations).at(-1), { source: EXECUTION_DETAIL_SOURCE, version: 3, rows: 6 });
    assert.equal(inspectAttempts, 4);
  } finally {
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
    if (Date.now() >= deadline) throw new Error('execution resource state did not settle');
    await new Promise((resolve) => setTimeout(resolve, 10));
  }
}

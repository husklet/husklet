import assert from 'node:assert/strict';
import { mkdtemp, rm } from 'node:fs/promises';
import net from 'node:net';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import { createElement as h } from 'react';
import { connect, workspace } from '../../../packages/react/src/index.js';
import { KIND, Reader, encode } from '../../../packages/react/src/wire.js';
import { Top } from '../dist/app.js';
import { ExecutionDetailsSource } from '../dist/model.js';
import { host } from './host.js';

test('same-id execution replacement fences detail output wait and consent', { timeout: 8_000 }, async () => {
  const directory = await mkdtemp(join(tmpdir(), 'husklet-execution-lifecycle-')); const socketPath = join(directory, 'host.sock');
  const id = 'e'.repeat(32); const container = 'c'.repeat(32); let lists = 0; const calls = [];
  const server = net.createServer((socket) => { const reader = new Reader(); socket.write(encode({ channel: 0, kind: KIND.open, payload: { protocol: 1, extension: 'execution-lifecycle-test', granted: ['containers:read', 'containers:control'] } })); socket.on('data', (chunk) => { for (const frame of reader.take(chunk)) {
    const call = frame.payload?.call; if (!call) continue; calls.push(call); let payload = { reply: 'done' };
    if (call === 'execution_list') { lists += 1; payload = { reply: 'executions', with: { executions: [{ id, container_id: container, running: true, exit_code: 0, pid: lists, command: [`generation-${lists}`], user: 'root' }], truncated: false } }; }
    else if (call === 'execution_inspect' || call === 'execution_wait') payload = { reply: 'execution', with: { id, container_id: container, running: true, exit_code: 0, pid: 1, command: ['stale-detail'], user: 'root' } };
    else if (call === 'execution_logs') payload = { reply: 'logs', with: { stdout: [...new TextEncoder().encode('stale-output')], stderr: [], truncated: false, stdout_truncated: false, stderr_truncated: false, eof: false } };
    const delayed = ['execution_inspect', 'execution_logs', 'execution_wait'].includes(call);
    setTimeout(() => socket.write(encode({ channel: frame.channel, kind: KIND.response, flags: 1, payload })), delayed ? 120 : 20);
  } }); });
  await new Promise((resolve, reject) => server.listen(socketPath, (error) => error ? reject(error) : resolve()));
  let session; let raceSession; let stage;
  try {
    session = await connect({ path: socketPath }); raceSession = await connect({ path: socketPath }); const framed = workspace(session); const race = workspace(raceSession); const mutations = [];
    stage = host(); stage.render(h(Top, { api: { ...framed, containers: { ...framed.containers, execution: race.containers.execution, executionLogs: race.containers.executionLogs, waitExecution: race.containers.waitExecution }, subscribe: undefined, unsubscribe: undefined }, executionDetails: new ExecutionDetailsSource(async (mutation) => mutations.push(mutation)), initial: { containers: [], images: [], volumes: [], networks: [] } }));
    invoke(stage, 'Executions'); await until(() => labelled(stage, 'generation-1'));
    invoke(stage, 'Details'); await until(() => calls.includes('execution_inspect')); invoke(stage, 'Terminate'); assert.ok(labelled(stage, `Send SIGTERM to execution ${id}?`));
    const start = stage.frames.length; invoke(stage, 'Refresh'); await until(() => labelled(stage, 'generation-2')); await delay(140);
    assert.equal(lengths(mutations), 0); assert.ok(stage.frames.slice(start).flatMap((frame) => frame.patches).some((patch) => 'Remove' in patch));
    assert.equal(stage.frames.slice(start).flatMap((frame) => frame.patches).some((patch) => patch.SetProp?.value?.Text === `Send SIGTERM to execution ${id}?`), false);

    invoke(stage, 'Details'); await until(() => lengths(mutations) === 1); invoke(stage, 'Load output'); await until(() => calls.filter((call) => call === 'execution_logs').length === 1);
    invoke(stage, 'Refresh'); await until(() => labelled(stage, 'generation-3')); await delay(140); assert.equal(labelled(stage, 'stale-output'), undefined);

    invoke(stage, 'Details'); await until(() => lengths(mutations) === 2); invoke(stage, 'Wait up to 5s'); await until(() => calls.filter((call) => call === 'execution_wait').length === 1);
    invoke(stage, 'Refresh'); await until(() => labelled(stage, 'generation-4')); await delay(140); assert.equal(lengths(mutations), 2); assert.equal(lists, 4, 'stale wait cannot trigger another inventory reload');
  } finally { stage?.render(null); await delay(30); session?.close(); raceSession?.close(); await new Promise((resolve) => server.close(resolve)); await rm(directory, { recursive: true, force: true }); }
});

function lengths(mutations) { return mutations.filter((mutation) => mutation.Length).length; }
function labelled(stage, label) { return stage.frames.flatMap((frame) => frame.patches).filter((patch) => patch.SetProp?.prop === 'Label' && patch.SetProp.value?.Text === label).at(-1); }
function invoke(stage, label) { const nodes = stage.frames.flatMap((frame) => frame.patches).filter((patch) => patch.SetProp?.prop === 'Label' && patch.SetProp.value?.Text === label).map((patch) => patch.SetProp.id).reverse(); assert.ok(nodes.some((node) => stage.surface.dispatch({ trigger: 'Invoke', node, id: `${node}:Invoke`, value: null })), `${label} invokes`); }
function delay(ms) { return new Promise((resolve) => setTimeout(resolve, ms)); }
async function until(done) { const deadline = Date.now() + 2_000; while (!done()) { if (Date.now() >= deadline) throw new Error('execution lifecycle did not settle'); await delay(10); } }

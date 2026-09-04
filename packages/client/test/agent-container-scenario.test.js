import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import net from 'node:net';
import os from 'node:os';
import path from 'node:path';
import { mkdtemp, rm } from 'node:fs/promises';
import test from 'node:test';
import { fileURLToPath } from 'node:url';
import { CONTROL, KIND, Reader, encode } from '../src/wire.js';

const example = fileURLToPath(new URL('../examples/agent-container-control.mjs', import.meta.url));

test('packaged external agent restores container after exact execution lifecycle over Unix framing', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-container-scenario-'));
  const socketPath = path.join(directory, 'host.sock'); const calls = []; const connections = new Set();
  const containerId = 'c'.repeat(64); const executionId = 'e'.repeat(32);
  const container = (state) => ({ id: containerId, name: 'agent', image: 'alpine', state, created: 1, generation: 4 });
  const execution = { id: executionId, container_id: containerId, running: false, exit_code: 0, pid: 21, command: ['printf', 'ok'], user: 'root' };
  const server = net.createServer((socket) => {
    connections.add(socket); socket.on('close', () => connections.delete(socket)); const reader = new Reader();
    socket.on('data', (chunk) => { for (const frame of reader.take(chunk)) {
      if (frame.channel !== 2) continue; const call = frame.payload.call; calls.push(call);
      if (call === 'workspace_info') {
        socket.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'workspace', with: { name: 'demo', image: 'alpine', architecture: 'amd64' } } }));
      } else if (call === 'container_inspect') {
        assert.deepEqual(frame.payload.with, { id: containerId });
        socket.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'container', with: container('exited') } }));
      } else if (call === 'event_subscribe' || call === 'event_unsubscribe') {
        socket.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
      } else if (call === 'container_start' || call === 'container_stop') {
        assert.deepEqual(frame.payload.with, { id: containerId });
        socket.write(encode({ channel: call === 'container_start' ? 210 : 211, kind: KIND.event, payload: {
          snapshot: 'containers', of: [container(call === 'container_start' ? 'running' : 'exited')],
        } }));
        socket.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
      } else if (call === 'container_exec') {
        assert.deepEqual(frame.payload.with, { id: containerId, command: ['printf', 'ok'], user: null, working_directory: null });
        socket.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'identity', with: executionId } }));
      } else if (call === 'execution_wait') {
        assert.deepEqual(frame.payload.with, { id: executionId, timeout_ms: 1000 });
        socket.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'execution', with: execution } }));
      } else if (call === 'execution_logs') {
        socket.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'logs', with: {
          stdout: [111, 107], stderr: [], truncated: false, stdout_truncated: false, stderr_truncated: false, eof: true,
        } } }));
      } else if (call === 'execution_remove') {
        assert.deepEqual(frame.payload.with, { id: executionId });
        socket.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
      }
    } });
    socket.write(encode({ channel: CONTROL, kind: KIND.open, payload: { protocol: 1, peer: 'container-agent', granted: ['workspaces:read', 'containers:read', 'containers:control'] } }));
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const child = spawn(process.execPath, [example, JSON.stringify({ path: socketPath, containerId, command: ['printf', 'ok'] })], { stdio: ['ignore', 'pipe', 'pipe'] });
    let stdout = ''; let stderr = ''; child.stdout.setEncoding('utf8'); child.stderr.setEncoding('utf8');
    child.stdout.on('data', (chunk) => { stdout += chunk; }); child.stderr.on('data', (chunk) => { stderr += chunk; });
    const code = await new Promise((resolve) => child.on('close', resolve));
    assert.equal(code, 0, stderr); const result = JSON.parse(stdout);
    assert.equal(result.workspace, 'demo'); assert.equal(result.container, containerId);
    assert.equal(result.execution.id, executionId); assert.deepEqual(result.output.stdout, [111, 107]);
    assert.deepEqual(calls, [
      'workspace_info', 'container_inspect',
      'event_subscribe', 'container_start', 'event_unsubscribe',
      'container_exec', 'execution_wait', 'execution_logs', 'execution_remove',
      'event_subscribe', 'container_stop', 'event_unsubscribe',
    ]);
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve)); await rm(directory, { recursive: true, force: true });
  }
});

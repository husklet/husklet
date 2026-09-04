import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import net from 'node:net';
import os from 'node:os';
import path from 'node:path';
import { mkdtemp, rm } from 'node:fs/promises';
import test from 'node:test';
import { fileURLToPath } from 'node:url';
import { CONTROL, KIND, Reader, encode } from '../src/wire.js';

const example = fileURLToPath(new URL('../examples/agent-workspace-control.mjs', import.meta.url));

test('packaged external agent arms every workspace lifecycle mutation over Unix framing', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-workspace-scenario-'));
  const socketPath = path.join(directory, 'host.sock'); const calls = []; const connections = new Set();
  const name = 'agent-e2e'; const generation = '0123456789abcdef0123456789abcdef';
  let revision = 40; let subscriptions = 0;
  const created = {
    name, image: 'alpine:3.20', architecture: 'amd64', generation,
    storage: null, shell: null, cpus: null, memory_mb: null, environment: [], mounts: [], docker_socket: false,
    scrollback: null, vpn: null, execution_lifetime: 'persisted',
    terminal: { font_family: null, font_size: null, foreground: null, background: null, cursor_shape: null, cursor_blink: null },
  };
  const actions = { workspace_create: 'create', workspace_start: 'start', workspace_stop: 'stop', workspace_delete: 'remove' };
  const server = net.createServer((socket) => {
    connections.add(socket); socket.on('close', () => connections.delete(socket)); const reader = new Reader();
    socket.on('data', (chunk) => { for (const frame of reader.take(chunk)) {
      if (frame.channel !== 2) continue; const call = frame.payload.call; calls.push(call);
      if (call === 'event_subscribe' || call === 'event_unsubscribe') {
        assert.deepEqual(frame.payload.with, { topic: 'workspace-lifecycle' });
        socket.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'done' } }));
        if (call === 'event_subscribe' && ++subscriptions > 1) socket.write(encode({ channel: 250 + subscriptions, kind: KIND.event, payload: {
          snapshot: 'workspace_lifecycle', of: { workspace: name, action: ['create', 'start', 'stop', 'remove'][subscriptions - 1], revision, coalesced: 0 },
        } }));
      } else if (actions[call]) {
        if (call === 'workspace_create') assert.equal(frame.payload.with.configuration.name, name);
        else if (call === 'workspace_delete') assert.deepEqual(frame.payload.with, { name, generation });
        else assert.deepEqual(frame.payload.with, { name });
        revision += 1;
        socket.write(encode({ channel: 300 + revision, kind: KIND.event, payload: { snapshot: 'workspace_lifecycle', of: {
          workspace: name, action: actions[call], revision, coalesced: 0,
        } } }));
        socket.write(encode({ channel: 2, kind: KIND.response, payload: call === 'workspace_create'
          ? { reply: 'workspace_configuration', with: created } : { reply: 'done' } }));
      }
    } });
    socket.write(encode({ channel: CONTROL, kind: KIND.open, payload: { protocol: 1, peer: 'workspace-agent', granted: ['workspaces:read', 'workspaces:control'] } }));
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  try {
    const child = spawn(process.execPath, [example, JSON.stringify({ path: socketPath, name, image: 'alpine:3.20', architecture: 'amd64' })], { stdio: ['ignore', 'pipe', 'pipe'] });
    let stdout = ''; let stderr = ''; child.stdout.setEncoding('utf8'); child.stderr.setEncoding('utf8');
    child.stdout.on('data', (chunk) => { stdout += chunk; }); child.stderr.on('data', (chunk) => { stderr += chunk; });
    const code = await new Promise((resolve) => child.on('close', resolve));
    assert.equal(code, 0, stderr); assert.deepEqual(JSON.parse(stdout), { name, generation, revision: 44 });
    assert.deepEqual(calls, [
      'event_subscribe', 'workspace_create', 'event_unsubscribe',
      'event_subscribe', 'workspace_start', 'event_unsubscribe',
      'event_subscribe', 'workspace_stop', 'event_unsubscribe',
      'event_subscribe', 'workspace_delete', 'event_unsubscribe',
    ]);
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve)); await rm(directory, { recursive: true, force: true });
  }
});

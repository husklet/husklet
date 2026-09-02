import assert from 'node:assert/strict';
import { mkdtemp, rm } from 'node:fs/promises';
import net from 'node:net';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { spawn } from 'node:child_process';
import test from 'node:test';
import { KIND, Reader, encode } from '../../react/src/wire.js';

test('the production entrypoint handshakes and renders through a real Unix socket', { timeout: 8_000 }, async () => {
  const directory = await mkdtemp(join(tmpdir(), 'husklet-workspace-manager-'));
  const socketPath = join(directory, 'host.sock');
  const calls = [];
  const server = net.createServer((socket) => {
    const reader = new Reader();
    socket.write(encode({ channel: 0, kind: KIND.request, payload: {
      protocol: 1, extension: 'workspace-manager', granted: ['container-read', 'container-control', 'image-read', 'image-write', 'volume-read', 'volume-write', 'network-read', 'network-write', 'interface'],
    } }));
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        const name = frame.payload?.call;
        if (!name) continue;
        calls.push(name);
        const payload = name === 'container_list'
          ? { reply: 'containers', with: [{ id: 'c1', name: 'api', image: 'alpine:3.20', state: 'running', created: 0 }] }
          : name === 'image_list'
            ? { reply: 'images', with: [{ id: 'i1', reference: 'alpine:3.20', size: 7, created: 0 }] }
            : name === 'volume_list'
              ? { reply: 'volumes', with: [{ name: 'cache', driver: 'local' }] }
              : name === 'network_list'
                ? { reply: 'networks', with: [{ id: 'n1', name: 'private', driver: 'bridge', scope: 'local' }] }
            : { reply: 'done' };
        socket.write(encode({ channel: 2, kind: KIND.response, payload }));
      }
    });
  });
  await new Promise((resolve, reject) => { server.once('error', reject); server.listen(socketPath, resolve); });
  const child = spawn(process.execPath, ['src/main.js'], {
    cwd: new URL('..', import.meta.url), env: { ...process.env, HUSKLET_EXTENSION_SOCKET: socketPath }, stdio: ['ignore', 'pipe', 'pipe'],
  });
  let stderr = '';
  child.stderr.on('data', (chunk) => { stderr += chunk; });
  try {
    await until(() => calls.includes('interface_open_tab') && calls.includes('interface_render') && calls.includes('container_list') && calls.includes('image_list') && calls.includes('volume_list') && calls.includes('network_list') && calls.filter((name) => name === 'event_subscribe').length === 4);
    assert.equal(stderr, '');
  } finally {
    child.kill('SIGTERM');
    await new Promise((resolve) => child.once('close', resolve));
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

async function until(done) {
  const deadline = Date.now() + 5_000;
  while (!done()) {
    if (Date.now() >= deadline) throw new Error('entrypoint did not reach the host calls');
    await new Promise((resolve) => setTimeout(resolve, 20));
  }
}

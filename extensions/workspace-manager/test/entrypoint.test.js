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
  const requests = [];
  let peer;
  let tearingDown = false;
  const server = net.createServer((socket) => {
    peer = socket;
    const reader = new Reader();
    socket.on('error', (error) => {
      if (!tearingDown) throw error;
      assert.equal(error.code, 'ECONNRESET');
    });
    socket.write(encode({ channel: 0, kind: KIND.request, payload: {
      protocol: 1, extension: 'workspace-manager', granted: ['container-read', 'container-control', 'image-read', 'image-write', 'volume-read', 'volume-write', 'network-read', 'network-write', 'interface'],
    } }));
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        const name = frame.payload?.call;
        if (!name) continue;
        calls.push(name);
        requests.push(frame.payload);
        const payload = name === 'interface_open_tab'
          ? { reply: 'identity', with: 'workspace-resources' }
          : name === 'container_list'
          ? { reply: 'containers', with: [{ id: 'c1', name: 'api', image: 'alpine:3.20', state: 'running', created: 0 }] }
          : name === 'container_inspect'
            ? { reply: 'container', with: { id: 'c1', name: 'api', image: 'alpine:3.20', state: 'running', created: 0 } }
          : name === 'image_list'
            ? { reply: 'images', with: [{ id: 'i1', reference: 'alpine:3.20', size: 7, created: 0 }] }
            : name === 'image_inspect'
              ? { reply: 'image_details', with: { id: 'i1', references: ['alpine:3.20'], created: 'now', size: 7, os: 'linux', architecture: 'amd64', entrypoint: ['/bin/sh'], command: [], working_directory: '/', user: '' } }
            : name === 'volume_list'
              ? { reply: 'volumes', with: [{ name: 'cache', driver: 'local' }] }
              : name === 'network_list'
                ? { reply: 'networks', with: [{ id: 'n1', name: 'private', driver: 'bridge', scope: 'local' }] }
            : { reply: 'done' };
        socket.write(encode({ channel: frame.channel, kind: KIND.response, payload }));
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
    await until(() => calls.includes('interface_open_tab') && calls.includes('interface_render_at') && calls.includes('container_list') && calls.includes('image_list') && calls.includes('volume_list') && calls.includes('network_list') && calls.filter((name) => name === 'event_subscribe').length === 4);
    const openingRenders = requests.filter((request) => request.call === 'interface_render_at').length;
    peer.write(encode({ channel: 9, kind: KIND.event, payload: invocation(requests, 'Images') }));
    await until(() => requests.filter((request) => request.call === 'interface_render_at').length > openingRenders);
    peer.write(encode({ channel: 10, kind: KIND.event, payload: invocation(requests, 'Inspect') }));
    await until(() => calls.includes('image_inspect') && calls.includes('source_resize_at'));
    const resize = requests.findLast((request) => request.call === 'source_resize_at');
    assert.deepEqual(resize.with.mutation.Length, { source: 201, version: 1, rows: 9 });
    const imageRenders = requests.filter((request) => request.call === 'interface_render_at').length;
    peer.write(encode({ channel: 11, kind: KIND.event, payload: invocation(requests, 'Containers') }));
    await until(() => requests.filter((request) => request.call === 'interface_render_at').length > imageRenders);
    peer.write(encode({ channel: 12, kind: KIND.event, payload: invocation(requests, 'Details') }));
    await until(() => calls.includes('container_inspect') && requests.some((request) =>
      request.call === 'source_resize_at' && request.with.mutation.Length?.source === 202));
    const containerResize = requests.findLast((request) => request.call === 'source_resize_at'
      && request.with.mutation.Length?.source === 202);
    assert.deepEqual(containerResize.with.mutation.Length, { source: 202, version: 1, rows: 5 });
    assert.equal(stderr, '');
  } finally {
    tearingDown = true;
    child.kill('SIGTERM');
    await new Promise((resolve) => child.once('close', resolve));
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

function invocation(requests, label) {
  const patches = requests.filter((request) => request.call === 'interface_render_at')
    .flatMap((request) => request.with.frame.patches);
  const labelled = patches.filter((patch) => patch.SetProp?.prop === 'Label' && patch.SetProp.value?.Text === label);
  assert.ok(labelled.length, `${label} is present on the live socket surface`);
  const handler = patches.findLast((patch) => patch.SetHandler?.handler?.trigger === 'Invoke'
    && labelled.some((candidate) => candidate.SetProp.id === patch.SetHandler.id));
  assert.ok(handler, `${label} advertises Invoke`);
  return { slot: 'workspace-resources', event: 'Invoke', node: handler.SetHandler.id, id: handler.SetHandler.handler.id };
}

async function until(done) {
  const deadline = Date.now() + 5_000;
  while (!done()) {
    if (Date.now() >= deadline) throw new Error('entrypoint did not reach the host calls');
    await new Promise((resolve) => setTimeout(resolve, 20));
  }
}

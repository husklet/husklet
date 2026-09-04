import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { mkdtemp, rm } from 'node:fs/promises';
import net from 'node:net';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import { KIND, Reader, encode } from '../../../packages/react/src/wire.js';

test('the Vite production entrypoint lists and renders Extensions over a Unix socket', { timeout: 8_000 }, async () => {
  const directory = await mkdtemp(join(tmpdir(), 'husklet-extensions-'));
  const socketPath = join(directory, 'host.sock');
  const calls = [];
  const server = net.createServer((socket) => {
    const reader = new Reader();
    socket.on('error', (error) => { if (error.code !== 'ECONNRESET') throw error; });
    socket.write(encode({ channel: 0, kind: KIND.open, payload: {
      protocol: 1, extension: 'extensions', granted: ['extensions:read', 'extensions:control', 'extensions:install', 'interface:render'],
    } }));
    socket.on('data', (chunk) => { for (const frame of reader.take(chunk)) {
      const call = frame.payload?.call;
      if (!call) continue;
      calls.push(frame.payload);
      const payload = call === 'extension_list'
        ? { reply: 'extensions', with: [{ name: 'resources', image_digest: `sha256:${'a'.repeat(64)}`, status: 'running', version: '0.1.0', enabled: true, pane_providers: [] }] }
        : call === 'interface_open_tab'
          ? { reply: 'identity', with: 'extensions-catalogue' }
          : { reply: 'done' };
      socket.write(encode({ channel: frame.channel, kind: KIND.response, flags: 1, payload }));
    } });
  });
  await new Promise((resolve, reject) => { server.once('error', reject); server.listen(socketPath, resolve); });
  const child = spawn(process.execPath, ['dist/main.js'], {
    cwd: new URL('..', import.meta.url), env: { ...process.env, HUSKLET_EXTENSION_SOCKET: socketPath }, stdio: ['ignore', 'ignore', 'pipe'],
  });
  let stderr = '';
  child.stderr.on('data', (chunk) => { stderr += chunk; });
  try {
    try {
      await until(() => calls.some(({ call }) => call === 'event_subscribe') && calls.some(({ call }) => call === 'interface_render_at'));
    } catch (error) {
      throw new Error(`${error.message}; calls=${JSON.stringify(calls.map(({ call }) => call))}; stderr=${JSON.stringify(stderr)}`);
    }
    assert.ok(renderedLabels(calls).includes('Extensions'));
    assert.ok(renderedLabels(calls).includes('resources'));
    assert.deepEqual(calls.find(({ call }) => call === 'event_subscribe').with, { topic: 'extensions' });
    assert.equal(stderr, '');
  } finally {
    const closed = child.exitCode === null ? new Promise((resolve) => child.once('close', resolve)) : Promise.resolve();
    child.kill('SIGTERM'); await closed;
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

function renderedLabels(calls) {
  return calls.filter(({ call }) => call === 'interface_render_at').flatMap(({ with: body }) => body.frame.patches)
    .flatMap((patch) => patch.SetProp?.prop === 'Label' ? [patch.SetProp.value?.Text] : []);
}

async function until(done) {
  const deadline = Date.now() + 5_000;
  while (!done()) { if (Date.now() >= deadline) throw new Error('entrypoint did not render'); await new Promise((resolve) => setTimeout(resolve, 20)); }
}

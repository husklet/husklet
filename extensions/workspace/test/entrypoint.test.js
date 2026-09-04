import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { mkdtemp, rm } from 'node:fs/promises';
import net from 'node:net';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import { KIND, Reader, encode } from '../../../packages/react/src/wire.js';

test('the Vite production entrypoint reads and renders Workspace over a Unix socket', { timeout: 8_000 }, async () => {
  const directory = await mkdtemp(join(tmpdir(), 'husklet-workspace-'));
  const socketPath = join(directory, 'host.sock');
  const calls = [];
  const server = net.createServer((socket) => {
    const reader = new Reader();
    socket.write(encode({ channel: 0, kind: KIND.open, payload: {
      protocol: 1, extension: 'workspace', granted: ['workspaces:read', 'workspaces:control', 'interface:render'],
    } }));
    socket.on('data', (chunk) => { for (const frame of reader.take(chunk)) {
      const call = frame.payload?.call;
      if (!call) continue;
      calls.push(frame.payload);
      const payload = call === 'workspace_info'
        ? { reply: 'workspace', with: { name: 'daily', architecture: 'amd64', image: 'alpine:3.20' } }
        : call === 'workspace_inspect'
          ? { reply: 'workspace_configuration', with: configuration() }
          : call === 'interface_open_tab'
            ? { reply: 'identity', with: 'workspace-settings' }
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
    await until(() => calls.some(({ call }) => call === 'workspace_inspect') && calls.some(({ call }) => call === 'interface_render_at'));
    assert.deepEqual(calls.find(({ call }) => call === 'workspace_inspect').with, { name: 'daily' });
    assert.ok(renderedLabels(calls).includes('Workspace'));
    assert.ok(renderedLabels(calls).includes('Environment'));
    assert.equal(stderr, '');
  } finally {
    const closed = child.exitCode === null ? new Promise((resolve) => child.once('close', resolve)) : Promise.resolve();
    child.kill('SIGTERM'); await closed;
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

function configuration() {
  return {
    generation: 'a'.repeat(32), name: 'daily', architecture: 'amd64', image: 'alpine:3.20', storage: null,
    shell: '/bin/sh', cpus: 2, memory_mb: 1024, environment: [], mounts: [], docker_socket: false,
    scrollback: 10000, vpn: null, execution_lifetime: 'live',
    terminal: { font_family: null, font_size: null, foreground: '#eeeeec', background: '#1e1e1e', cursor_shape: null, cursor_blink: false },
  };
}

function renderedLabels(calls) {
  return calls.filter(({ call }) => call === 'interface_render_at').flatMap(({ with: body }) => body.frame.patches)
    .flatMap((patch) => patch.SetProp?.prop === 'Label' ? [patch.SetProp.value?.Text] : []);
}

async function until(done) {
  const deadline = Date.now() + 5_000;
  while (!done()) { if (Date.now() >= deadline) throw new Error('entrypoint did not render'); await new Promise((resolve) => setTimeout(resolve, 20)); }
}

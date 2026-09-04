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
  let peer;
  let acquisitions = 0;
  let cancelled = false;
  const server = net.createServer((socket) => {
    peer = socket;
    const reader = new Reader();
    socket.on('error', (error) => { if (error.code !== 'ECONNRESET') throw error; });
    socket.write(encode({ channel: 0, kind: KIND.open, payload: {
      protocol: 1, extension: 'extensions', granted: ['extensions:read', 'extensions:control', 'extensions:install', 'interface:render'],
    } }));
    socket.on('data', (chunk) => { for (const frame of reader.take(chunk)) {
      const call = frame.payload?.call;
      if (!call) continue;
      calls.push(frame.payload);
      if (call === 'extension_acquisition_start') acquisitions += 1;
      if (call === 'extension_acquisition_cancel') cancelled = true;
      const payload = call === 'extension_list'
        ? { reply: 'extensions', with: [
          { name: 'resources', image_digest: `sha256:${'a'.repeat(64)}`, status: 'duty', version: '0.1.0', enabled: true, pane_providers: [] },
          { name: 'broken', image_digest: `sha256:${'c'.repeat(64)}`, status: 'fault:3', version: '0.1.0', enabled: true, pane_providers: [] },
          { name: 'paused', image_digest: `sha256:${'d'.repeat(64)}`, status: 'standby', version: '0.1.0', enabled: false, pane_providers: [] },
        ] }
        : call === 'extension_acquisition_start'
          ? { reply: 'extension_acquisition_job', with: { job: `job-${acquisitions}` } }
          : call === 'extension_acquisition_status'
            ? acquisitions === 1 ? { reply: 'extension_acquisition', with: {
              job: 'job-1', reference: 'registry.example/extension:2', revision: 3, state: 'ready',
              candidate: { name: 'resources', version: '2.0.0', image_digest: `sha256:${'b'.repeat(64)}`, installed_image_digest: `sha256:${'a'.repeat(64)}`, requested: ['containers:read', 'containers:control'] },
            } } : { reply: 'extension_acquisition', with: { job: 'job-2', reference: 'registry.example/slow:1', revision: 4, state: cancelled ? 'cancelled' : 'downloading' } }
            : call === 'extension_update'
              ? { reply: 'extension', with: { name: 'resources', image_digest: `sha256:${'b'.repeat(64)}`, status: 'running' } }
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
    assert.ok(renderedLabels(calls).includes('Disable'), 'a duty extension can be disabled');
    assert.ok(renderedLabels(calls).includes('Retry'), 'a faulted extension can be retried');
    assert.ok(renderedLabels(calls).includes('Enable'), 'a standby extension can be enabled');
    assert.deepEqual(calls.find(({ call }) => call === 'event_subscribe').with, { topic: 'extensions' });
    peer.write(encode({ channel: 20, kind: KIND.event, payload: invoke(calls, 'Disable') }));
    await until(() => calls.some(({ call }) => call === 'extension_disable'));
    assert.deepEqual(calls.find(({ call }) => call === 'extension_disable').with, {
      name: 'resources', image_digest: `sha256:${'a'.repeat(64)}`,
    });
    peer.write(encode({ channel: 21, kind: KIND.event, payload: invoke(calls, 'Retry') }));
    await until(() => calls.some(({ call }) => call === 'extension_retry'));
    assert.deepEqual(calls.find(({ call }) => call === 'extension_retry').with, {
      name: 'broken', image_digest: `sha256:${'c'.repeat(64)}`,
    });
    peer.write(encode({ channel: 22, kind: KIND.event, payload: invoke(calls, 'Enable') }));
    await until(() => calls.some(({ call }) => call === 'extension_enable'));
    assert.deepEqual(calls.find(({ call }) => call === 'extension_enable').with, {
      name: 'paused', image_digest: `sha256:${'d'.repeat(64)}`,
    });
    peer.write(encode({ channel: 7, kind: KIND.event, payload: change(calls, 'registry.example/extension:version', 'registry.example/extension:2') }));
    await until(() => renderedLabels(calls).includes('Inspect'));
    peer.write(encode({ channel: 8, kind: KIND.event, payload: invoke(calls, 'Inspect') }));
    await until(() => renderedLabels(calls).includes('Update extension'));
    peer.write(encode({ channel: 9, kind: KIND.event, payload: toggle(calls, 0, false) }));
    peer.write(encode({ channel: 10, kind: KIND.event, payload: invoke(calls, 'Update extension') }));
    await until(() => calls.some(({ call }) => call === 'extension_update'));
    assert.deepEqual(calls.find(({ call }) => call === 'extension_update').with, {
      job: 'job-1', revision: 3, granted: ['containers:control'],
    });
    const beforeSlow = calls.filter(({ call }) => call === 'interface_render_at').length;
    peer.write(encode({ channel: 11, kind: KIND.event, payload: change(calls, 'registry.example/extension:version', 'registry.example/slow:1') }));
    await until(() => calls.filter(({ call }) => call === 'interface_render_at').length > beforeSlow);
    peer.write(encode({ channel: 12, kind: KIND.event, payload: invoke(calls, 'Inspect') }));
    await until(() => renderedLabels(calls).includes('Cancel'));
    await until(() => calls.some(({ call, with: body }) => call === 'event_subscribe' && body.topic === 'extension-acquisitions'));
    peer.write(encode({ channel: 13, kind: KIND.event, payload: invoke(calls, 'Cancel') }));
    await until(() => calls.some(({ call }) => call === 'extension_acquisition_cancel'));
    assert.deepEqual(calls.find(({ call }) => call === 'extension_acquisition_cancel').with, { job: 'job-2', revision: 4 });
    assert.equal(calls.filter(({ call }) => call === 'extension_acquisition_status').length, 3,
      'one initial read per job plus the cancellation refresh replaces repeated polling');
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

function renderedPatches(calls) {
  return calls.filter(({ call }) => call === 'interface_render_at').flatMap(({ with: body }) => body.frame.patches);
}

function change(calls, placeholder, value) {
  const patches = renderedPatches(calls);
  const node = patches.findLast((patch) => patch.SetProp?.prop === 'Placeholder' && patch.SetProp.value?.Text === placeholder).SetProp.id;
  const handler = patches.findLast((patch) => patch.SetHandler?.id === node && patch.SetHandler.handler?.trigger === 'Change').SetHandler;
  return { slot: 'extensions-catalogue', event: 'Change', node, id: handler.handler.id, value };
}

function invoke(calls, label) {
  const patches = renderedPatches(calls);
  const node = patches.findLast((patch) => patch.SetProp?.prop === 'Label' && patch.SetProp.value?.Text === label).SetProp.id;
  const handler = patches.findLast((patch) => patch.SetHandler?.id === node && patch.SetHandler.handler?.trigger === 'Invoke').SetHandler;
  return { slot: 'extensions-catalogue', event: 'Invoke', node, id: handler.handler.id };
}

function toggle(calls, index, value) {
  const handlers = renderedPatches(calls).filter((patch) => patch.SetHandler?.handler?.trigger === 'Toggle');
  const handler = handlers[index].SetHandler;
  return { slot: 'extensions-catalogue', event: 'Toggle', node: handler.id, id: handler.handler.id, value };
}

async function until(done) {
  const deadline = Date.now() + 5_000;
  while (!done()) { if (Date.now() >= deadline) throw new Error('entrypoint did not render'); await new Promise((resolve) => setTimeout(resolve, 20)); }
}

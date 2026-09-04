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
  let failNextList = false;
  let responses = Promise.resolve();
  let installed = [
    { name: 'resources', image_digest: `sha256:${'a'.repeat(64)}`, status: 'duty', version: '0.1.0', enabled: true, pane_providers: [] },
    { name: 'broken', image_digest: `sha256:${'c'.repeat(64)}`, status: 'fault:3', version: '0.1.0', enabled: true, pane_providers: [] },
    { name: 'paused', image_digest: `sha256:${'d'.repeat(64)}`, status: 'standby', version: '0.1.0', enabled: false, pane_providers: [] },
  ];
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
      const firstList = call === 'extension_list' && calls.filter(({ call: seen }) => seen === 'extension_list').length === 1;
      const failingList = call === 'extension_list' && failNextList;
      if (failingList) failNextList = false;
      if (call === 'extension_acquisition_start') acquisitions += 1;
      if (call === 'extension_acquisition_cancel') cancelled = true;
      if (call === 'extension_disable') installed = installed.map((item) => item.name === frame.payload.with.name ? { ...item, status: 'standby', enabled: false } : item);
      if (call === 'extension_enable' || call === 'extension_retry') installed = installed.map((item) => item.name === frame.payload.with.name ? { ...item, status: 'duty', enabled: true } : item);
      if (call === 'extension_remove') installed = installed.filter((item) => item.name !== frame.payload.with.name || item.image_digest !== frame.payload.with.image_digest);
      if (call === 'extension_update') installed = installed.map((item) => item.name === 'resources' ? { ...item, image_digest: `sha256:${'b'.repeat(64)}`, version: '2.0.0', status: 'duty', enabled: true } : item);
      const payload = failingList
        ? { error: 'failed', detail: 'extension inventory unavailable' }
        : call === 'extension_list'
        ? { reply: 'extensions', with: installed }
        : call === 'extension_acquisition_start'
          ? { reply: 'extension_acquisition_job', with: { job: `job-${acquisitions}` } }
          : call === 'extension_acquisition_status'
            ? acquisitions === 1 ? { reply: 'extension_acquisition', with: {
              job: 'job-1', reference: 'registry.example/extension:2', revision: 3, state: 'ready',
              candidate: { name: 'resources', version: '2.0.0', image_digest: `sha256:${'b'.repeat(64)}`, installed_image_digest: `sha256:${'a'.repeat(64)}`, requested: ['containers:read', 'containers:control'] },
            } } : { reply: 'extension_acquisition', with: {
              job: 'job-2', reference: 'registry.example/slow:1', revision: 4, state: cancelled ? 'cancelled' : 'downloading',
              progress: cancelled ? null : { status: 'Pulling layers', id: 'layer-2', current: 1024, total: 4096 },
              candidate: { name: 'slow', version: '1.0.0', image_digest: `sha256:${'e'.repeat(64)}`, installed_image_digest: null, requested: ['containers:read'] },
            } }
            : call === 'extension_update'
              ? { reply: 'extension', with: { name: 'resources', image_digest: `sha256:${'b'.repeat(64)}`, status: 'running' } }
        : call === 'interface_open_tab'
          ? { reply: 'identity', with: 'extensions-catalogue' }
          : { reply: 'done' };
      const respond = () => {
        socket.write(encode({ channel: frame.channel, kind: KIND.response, flags: failingList ? 3 : 1, payload }));
        if (['extension_disable', 'extension_enable', 'extension_retry', 'extension_remove', 'extension_update'].includes(call)) {
          socket.write(encode({ channel: 30, kind: KIND.event, payload: { snapshot: 'extensions', of: installed } }));
        }
      };
      responses = responses.then(async () => { if (firstList) await new Promise((resolve) => setTimeout(resolve, 80)); respond(); });
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
    assert.ok(renderedLabels(calls).includes('Loading installed extensions…'));
    assert.equal(renderedLabels(calls).includes('No extensions installed'), false, 'loading never claims the inventory is empty');
    try { await until(() => renderedLabels(calls).includes('resources')); }
    catch (cause) { throw new Error(`${cause.message}; calls=${JSON.stringify(calls.map(({ call }) => call))}; stderr=${JSON.stringify(stderr)}`); }
    assert.ok(renderedLabels(calls).includes('resources'));
    assert.ok(renderedLabels(calls).includes('Refresh'));
    const initialResources = renderedLabels(calls).filter((label) => label === 'resources').length;
    let listCalls = calls.filter(({ call }) => call === 'extension_list').length;
    failNextList = true;
    peer.write(encode({ channel: 19, kind: KIND.event, payload: invoke(calls, 'Refresh') }));
    await until(() => calls.filter(({ call }) => call === 'extension_list').length > listCalls
      && renderedLabels(calls).includes('extension inventory unavailable'));
    assert.equal(renderedLabels(calls).includes('No extensions installed'), false, 'failure never claims an empty inventory');
    listCalls = calls.filter(({ call }) => call === 'extension_list').length;
    peer.write(encode({ channel: 19, kind: KIND.event, payload: invoke(calls, 'Retry') }));
    await until(() => calls.filter(({ call }) => call === 'extension_list').length > listCalls
      && renderedLabels(calls).filter((label) => label === 'resources').length > initialResources);
    assert.ok(renderedLabels(calls).includes('Disable'), 'a duty extension can be disabled');
    assert.ok(renderedLabels(calls).includes('Retry'), 'a faulted extension can be retried');
    assert.ok(renderedLabels(calls).includes('Enable'), 'a standby extension can be enabled');
    assert.deepEqual(calls.find(({ call }) => call === 'event_subscribe').with, { topic: 'extensions' });
    const enablePaused = invoke(calls, 'Enable');
    peer.write(encode({ channel: 20, kind: KIND.event, payload: invoke(calls, 'Disable') }));
    await until(() => calls.some(({ call }) => call === 'extension_disable'));
    assert.deepEqual(calls.find(({ call }) => call === 'extension_disable').with, {
      name: 'resources', image_digest: `sha256:${'a'.repeat(64)}`,
    });
    await until(() => renderedLabels(calls).includes('resources disabled and verified.'));
    peer.write(encode({ channel: 21, kind: KIND.event, payload: invoke(calls, 'Retry') }));
    await until(() => calls.some(({ call }) => call === 'extension_retry'));
    assert.deepEqual(calls.find(({ call }) => call === 'extension_retry').with, {
      name: 'broken', image_digest: `sha256:${'c'.repeat(64)}`,
    });
    await until(() => renderedLabels(calls).includes('broken recovered and verified.'));
    peer.write(encode({ channel: 22, kind: KIND.event, payload: enablePaused }));
    await until(() => calls.some(({ call }) => call === 'extension_enable'));
    assert.deepEqual(calls.find(({ call }) => call === 'extension_enable').with, {
      name: 'paused', image_digest: `sha256:${'d'.repeat(64)}`,
    });
    await until(() => renderedLabels(calls).includes('paused enabled and verified.'));
    peer.write(encode({ channel: 23, kind: KIND.event, payload: invoke(calls, 'Remove') }));
    await until(() => renderedLabels(calls).includes('Remove paused'));
    peer.write(encode({ channel: 24, kind: KIND.event, payload: invoke(calls, 'Remove paused') }));
    await until(() => calls.some(({ call }) => call === 'extension_remove'));
    assert.deepEqual(calls.find(({ call }) => call === 'extension_remove').with, {
      name: 'paused', image_digest: `sha256:${'d'.repeat(64)}`,
    });
    await until(() => renderedLabels(calls).includes('paused removed and verified.'));
    assert.equal(installed.some(({ name }) => name === 'paused'), false);
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
    await until(() => renderedLabels(calls).includes('resources updated and verified.'));
    const beforeSlow = calls.filter(({ call }) => call === 'interface_render_at').length;
    peer.write(encode({ channel: 11, kind: KIND.event, payload: change(calls, 'registry.example/extension:version', 'registry.example/slow:1') }));
    await until(() => calls.filter(({ call }) => call === 'interface_render_at').length > beforeSlow);
    peer.write(encode({ channel: 12, kind: KIND.event, payload: invoke(calls, 'Inspect') }));
    await until(() => renderedLabels(calls).includes('Pulling layers · layer-2 · 1024/4096 bytes'));
    assert.ok(renderedLabels(calls).includes('Cancel'));
    assert.equal(enabledState(calls, 'Install extension'), false, 'candidate metadata is not commit authority before ready');
    await until(() => calls.some(({ call, with: body }) => call === 'event_subscribe' && body.topic === 'extension-acquisitions'));
    peer.write(encode({ channel: 13, kind: KIND.event, payload: invoke(calls, 'Install extension') }));
    await new Promise((resolve) => setTimeout(resolve, 40));
    assert.equal(calls.some(({ call }) => call === 'extension_install'), false, 'a stale invoke cannot bypass ready state');
    peer.write(encode({ channel: 13, kind: KIND.event, payload: invoke(calls, 'Cancel') }));
    await until(() => calls.some(({ call }) => call === 'extension_acquisition_cancel'));
    assert.deepEqual(calls.find(({ call }) => call === 'extension_acquisition_cancel').with, { job: 'job-2', revision: 4 });
    assert.equal(calls.filter(({ call }) => call === 'extension_acquisition_status').length, 4,
      'initial reads, exact pre-commit reinspection, and cancellation refresh replace repeated polling');
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

function enabledState(calls, label) {
  const patches = renderedPatches(calls);
  const node = patches.findLast((patch) => patch.SetProp?.prop === 'Label' && patch.SetProp.value?.Text === label).SetProp.id;
  return patches.findLast((patch) => patch.SetProp?.id === node && patch.SetProp.prop === 'Enabled').SetProp.value.Flag;
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

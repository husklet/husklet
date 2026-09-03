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
  const received = [];
  let peer;
  let tearingDown = false;
  let executed = false;
  const containerId = 'a'.repeat(32);
  const executionId = 'c'.repeat(32);
  const networkId = 'b'.repeat(32);
  const server = net.createServer((socket) => {
    peer = socket;
    const reader = new Reader();
    socket.on('error', (error) => {
      if (!tearingDown) throw error;
      assert.equal(error.code, 'ECONNRESET');
    });
    socket.write(encode({ channel: 0, kind: KIND.request, payload: {
      protocol: 1, extension: 'workspace-manager', granted: ['container-read', 'container-control', 'container-attach', 'image-read', 'image-write', 'volume-read', 'volume-write', 'network-read', 'network-write', 'interface'],
    } }));
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        received.push(frame);
        const name = frame.payload?.call;
        if (!name) continue;
        calls.push(name);
        requests.push(frame.payload);
        const payload = name === 'interface_open_tab'
          ? { reply: 'identity', with: 'workspace-resources' }
          : name === 'container_list'
          ? { reply: 'containers', with: [{ id: containerId, name: 'api', image: 'alpine:3.20', state: 'running', created: 0 }] }
          : name === 'container_inspect'
            ? { reply: 'container', with: { id: containerId, name: 'api', image: 'alpine:3.20', state: 'running', created: 0 } }
          : name === 'container_create'
            ? { reply: 'identity', with: 'created-over-socket' }
          : name === 'container_exec'
            ? (executed = true, { reply: 'identity', with: executionId })
          : name === 'container_attach_terminal'
            ? { reply: 'identity', with: 'p-attached' }
          : name === 'execution_list'
            ? { reply: 'executions', with: { executions: [
              { id: 'e1', container_id: containerId, running: false, exit_code: 0, pid: 0, command: ['true'], user: '' },
              ...(executed ? [{ id: executionId, container_id: containerId, running: true, exit_code: 0, pid: 84, command: ['sh', '-lc', 'printf hello world'], user: '' }] : []),
            ], truncated: false } }
          : name === 'execution_inspect'
            ? { reply: 'execution', with: frame.payload.with.id === executionId
              ? { id: executionId, container_id: containerId, running: true, exit_code: 0, pid: 84, command: ['sh', '-lc', 'printf hello world'], user: '' }
              : { id: 'e1', container_id: containerId, running: false, exit_code: 0, pid: 0, command: ['true'], user: '' } }
          : name === 'execution_logs'
            ? { reply: 'logs', with: { stdout: [111, 107], stderr: [33], truncated: false } }
          : name === 'image_list'
            ? { reply: 'images', with: [{ id: 'i1', reference: 'alpine:3.20', size: 7, created: 0 }] }
            : name === 'image_pull_start'
              ? { reply: 'image_pull_job', with: { job: 'p1' } }
            : name === 'image_pull_status'
              ? { reply: 'image_pull', with: { job: 'p1', reference: 'alpine:3.20', revision: 2, state: 'pulling', status: 'Downloading', layer: 'layer1', current: 5, total: 10, image: null, error: null } }
            : name === 'image_inspect'
              ? { reply: 'image_details', with: { id: 'i1', references: ['alpine:3.20'], created: 'now', size: 7, os: 'linux', architecture: 'amd64', entrypoint: ['/bin/sh'], command: [], working_directory: '/', user: '' } }
            : name === 'volume_list'
              ? { reply: 'volumes', with: [{ name: 'cache', driver: 'local', generation: 'a'.repeat(32) }] }
            : name === 'volume_inspect'
              ? { reply: 'volume', with: { name: 'cache', driver: 'local', generation: 'a'.repeat(32) } }
              : name === 'network_create'
                ? { reply: 'identity', with: networkId }
                : name === 'volume_create'
                  ? { reply: 'volume', with: { name: frame.payload.with.name, driver: 'local', generation: 'b'.repeat(32) } }
              : name === 'network_list'
                ? { reply: 'networks', with: [{ id: networkId, name: 'private', driver: 'bridge', scope: 'local' }] }
              : name === 'network_inspect'
                ? { reply: 'network', with: { id: networkId, name: 'private', driver: 'bridge', scope: 'local' } }
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
    try {
      await until(() => calls.includes('interface_open_tab') && calls.includes('interface_render_at') && calls.includes('container_list') && calls.includes('execution_list') && calls.includes('image_list') && calls.includes('volume_list') && calls.includes('network_list') && calls.filter((name) => name === 'event_subscribe').length === 4);
    } catch (error) {
      throw new Error(`${error.message}; calls=${JSON.stringify(calls)} stderr=${JSON.stringify(stderr)}`);
    }
    const openingRenders = requests.filter((request) => request.call === 'interface_render_at').length;
    peer.write(encode({ channel: 9, kind: KIND.event, payload: invocation(requests, 'Images') }));
    await until(() => requests.filter((request) => request.call === 'interface_render_at').length > openingRenders);
    peer.write(encode({ channel: 23, kind: KIND.event, payload: changeInvocation(requests, 'registry/image:tag', 'alpine:3.20') }));
    await until(() => requests.some((request) => request.call === 'interface_render_at'
      && request.with.frame.patches.some((patch) => patch.SetProp?.prop === 'Value' && patch.SetProp.value?.Text === 'alpine:3.20')));
    peer.write(encode({ channel: 24, kind: KIND.event, payload: invocation(requests, 'Pull') }));
    await until(() => calls.includes('image_pull_start') && requests.some((request) => request.call === 'event_subscribe' && request.with.topic === 'image-pulls'));
    peer.write(encode({ channel: 78, kind: KIND.event, payload: { snapshot: 'image_pulls', of: { job: 'p1', revision: 2, state: 'pulling', coalesced: 0 } } }));
    await until(() => calls.includes('image_pull_status') && received.some((frame) => frame.channel === 78 && frame.kind === KIND.credit)
      && requests.some((request) => request.call === 'interface_render_at' && request.with.frame.patches.some((patch) => patch.SetProp?.value?.Text === 'Layer layer1')));
    peer.write(encode({ channel: 25, kind: KIND.event, payload: invocation(requests, 'Cancel pull') }));
    await until(() => calls.includes('image_pull_cancel') && requests.some((request) => request.call === 'event_unsubscribe' && request.with.topic === 'image-pulls'));
    peer.write(encode({ channel: 10, kind: KIND.event, payload: invocation(requests, 'Inspect') }));
    await until(() => calls.includes('image_inspect') && calls.includes('source_resize_at'));
    const resize = requests.findLast((request) => request.call === 'source_resize_at');
    assert.deepEqual(resize.with.mutation.Length, { source: 201, version: 1, rows: 9 });
    const imageRenders = requests.filter((request) => request.call === 'interface_render_at').length;
    peer.write(encode({ channel: 11, kind: KIND.event, payload: invocation(requests, 'Containers') }));
    await until(() => requests.filter((request) => request.call === 'interface_render_at').length > imageRenders);
    peer.write(encode({ channel: 34, kind: KIND.event, payload: changeInvocation(requests, `New name for ${containerId.slice(0, 12)}`, 'api-renamed') }));
    await until(() => requests.some((request) => request.call === 'interface_render_at'
      && request.with.frame.patches.some((patch) => patch.SetProp?.prop === 'Value' && patch.SetProp.value?.Text === 'api-renamed')));
    peer.write(encode({ channel: 35, kind: KIND.event, payload: invocation(requests, 'Rename') }));
    await until(() => calls.includes('container_rename'));
    assert.deepEqual(requests.find((request) => request.call === 'container_rename').with, { id: containerId, name: 'api-renamed' });
    peer.write(encode({ channel: 26, kind: KIND.event, payload: changeInvocation(requests, 'Image reference', 'alpine:3.20') }));
    peer.write(encode({ channel: 27, kind: KIND.event, payload: changeInvocation(requests, 'Container name', 'worker') }));
    await until(() => requests.some((request) => request.call === 'interface_render_at'
      && request.with.frame.patches.some((patch) => patch.SetProp?.value?.Text === 'worker')));
    peer.write(encode({ channel: 28, kind: KIND.event, payload: invocation(requests, 'Create and start') }));
    await until(() => calls.includes('container_create') && calls.includes('container_start'));
    assert.deepEqual(requests.find((request) => request.call === 'container_create').with.spec, {
      image: 'alpine:3.20', name: 'worker', entrypoint: null, command: [], environment: [], working_directory: null,
      hostname: null, user: null, labels: [], mounts: [], network: null, ports: [], memory_mb: null, cpus: null, pids_limit: null,
    });
    assert.deepEqual(requests.find((request) => request.call === 'container_start').with, { id: 'created-over-socket' });
    peer.write(encode({ channel: 12, kind: KIND.event, payload: invocation(requests, 'Details') }));
    await until(() => calls.includes('container_inspect') && requests.some((request) =>
      request.call === 'source_resize_at' && request.with.mutation.Length?.source === 202));
    const containerResize = requests.findLast((request) => request.call === 'source_resize_at'
      && request.with.mutation.Length?.source === 202);
    assert.deepEqual(containerResize.with.mutation.Length, { source: 202, version: 1, rows: 5 });
    peer.write(encode({ channel: 29, kind: KIND.event, payload: changeInvocation(requests, 'Command argv JSON', '["sh","-lc","printf hello world"]') }));
    peer.write(encode({ channel: 32, kind: KIND.event, payload: changeInvocation(requests, 'Run as user (optional)', '1000:1000') }));
    peer.write(encode({ channel: 33, kind: KIND.event, payload: changeInvocation(requests, 'Working directory (optional)', '/work tree') }));
    await until(() => ['["sh","-lc","printf hello world"]', '1000:1000', '/work tree'].every((value) =>
      requests.some((request) => request.call === 'interface_render_at'
        && request.with.frame.patches.some((patch) => patch.SetProp?.value?.Text === value))));
    peer.write(encode({ channel: 30, kind: KIND.event, payload: invocation(requests, 'Execute') }));
    await until(() => calls.includes('container_exec') && requests.some((request) => request.call === 'interface_render_at'
      && request.with.frame.patches.some((patch) => patch.SetProp?.value?.Text === `Execution ${executionId} created.`)));
    assert.deepEqual(requests.find((request) => request.call === 'container_exec').with, {
      id: containerId, command: ['sh', '-lc', 'printf hello world'], user: '1000:1000', working_directory: '/work tree',
    });
    peer.write(encode({ channel: 36, kind: KIND.event, payload: invocation(requests, 'Attach terminal') }));
    await until(() => calls.includes('container_attach_terminal'));
    assert.deepEqual(requests.find((request) => request.call === 'container_attach_terminal').with, {
      id: containerId, command: ['sh', '-lc', 'printf hello world'],
    });
    peer.write(encode({ channel: 31, kind: KIND.event, payload: invocation(requests, 'Inspect execution') }));
    await until(() => requests.some((request) => request.call === 'execution_inspect'
      && request.with.id === executionId));
    const beforeExecutions = requests.filter((request) => request.call === 'interface_render_at').length;
    peer.write(encode({ channel: 13, kind: KIND.event, payload: invocation(requests, 'Executions') }));
    await until(() => requests.some((request) => request.call === 'event_subscribe' && request.with.topic === 'executions'));
    const eventStart = received.length;
    peer.write(encode({ channel: 77, kind: KIND.event, payload: { snapshot: 'executions', of: {
      executions: [
        { id: 'e1', container_id: containerId, running: false, exit_code: 0, pid: 0, command: ['true'], user: '' },
        { id: 'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb', container_id: containerId, running: true, exit_code: 0, pid: 42, command: ['live-command'], user: 'root' },
      ], truncated: true,
    } } }));
    await until(() => received.some((frame) => frame.channel === 77 && frame.kind === KIND.credit)
      && requests.some((request) => request.call === 'interface_render_at'
        && request.with.frame.patches.some((patch) => patch.SetProp?.value?.Text === 'live-command')));
    const delivered = received.slice(eventStart);
    const renderIndex = delivered.findIndex((frame) => frame.payload?.call === 'interface_render_at'
      && frame.payload.with.frame.patches.some((patch) => patch.SetProp?.value?.Text === 'live-command'));
    const creditIndex = delivered.findIndex((frame) => frame.channel === 77 && frame.kind === KIND.credit);
    assert.ok(renderIndex >= 0 && creditIndex > renderIndex, 'credit follows delivery of the observed state');
    assert.ok(requests.some((request) => request.call === 'interface_render_at'
      && request.with.frame.patches.some((patch) => patch.SetProp?.value?.Text === 'The host execution catalogue was truncated at its safety limit.')));
    assert.ok(requests.filter((request) => request.call === 'interface_render_at').length > beforeExecutions);
    peer.write(encode({ channel: 34, kind: KIND.event, payload: invocation(requests, 'Terminate') }));
    await until(() => requests.some((request) => request.call === 'interface_render_at'
      && request.with.frame.patches.some((patch) => patch.SetProp?.value?.Text === 'Send SIGTERM to execution bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb?')));
    peer.write(encode({ channel: 35, kind: KIND.event, payload: invocation(requests, 'Confirm SIGTERM') }));
    await until(() => calls.includes('execution_kill'));
    assert.deepEqual(requests.find((request) => request.call === 'execution_kill').with, { id: 'bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb', signal: 'SIGTERM' });
    peer.write(encode({ channel: 14, kind: KIND.event, payload: invocation(requests, 'Details') }));
    await until(() => calls.includes('execution_inspect') && requests.some((request) =>
      request.call === 'source_resize_at' && request.with.mutation.Length?.source === 203));
    peer.write(encode({ channel: 15, kind: KIND.event, payload: invocation(requests, 'Load output') }));
    await until(() => calls.includes('execution_logs'));
    peer.write(encode({ channel: 16, kind: KIND.event, payload: invocation(requests, 'Remove record') }));
    await until(() => requests.some((request) => request.call === 'interface_render_at'
      && request.with.frame.patches.some((patch) => patch.SetProp?.value?.Text === 'Confirm removal')));
    peer.write(encode({ channel: 17, kind: KIND.event, payload: invocation(requests, 'Confirm removal') }));
    await until(() => calls.includes('execution_remove'));
    peer.write(encode({ channel: 18, kind: KIND.event, payload: invocation(requests, 'Images') }));
    await until(() => requests.some((request) => request.call === 'event_unsubscribe' && request.with.topic === 'executions'));
    peer.write(encode({ channel: 19, kind: KIND.event, payload: invocation(requests, 'Networks') }));
    await until(() => requests.some((request) => request.call === 'interface_render_at'
      && request.with.frame.patches.some((patch) => patch.SetProp?.value?.Text === 'Complete container ID')));
    peer.write(encode({ channel: 36, kind: KIND.event, payload: changeInvocation(requests, 'Network name', 'socket-net') }));
    await until(() => requests.some((request) => request.call === 'interface_render_at'
      && request.with.frame.patches.some((patch) => patch.SetProp?.value?.Text === 'socket-net')));
    peer.write(encode({ channel: 37, kind: KIND.event, payload: invocation(requests, 'Create') }));
    await until(() => calls.includes('network_create') && requests.some((request) => request.call === 'interface_render_at'
      && request.with.frame.patches.some((patch) => patch.SetProp?.value?.Text === 'Created network socket-net.')));
    assert.deepEqual(requests.find((request) => request.call === 'network_create').with, { name: 'socket-net' });
    peer.write(encode({ channel: 38, kind: KIND.event, payload: changeInvocation(requests, 'Complete container ID', containerId) }));
    peer.write(encode({ channel: 39, kind: KIND.event, payload: changeInvocation(requests, 'Endpoint aliases (comma-separated, optional)', 'database.internal, database_2') }));
    await until(() => requests.some((request) => request.call === 'interface_render_at'
      && request.with.frame.patches.some((patch) => patch.SetProp?.value?.Text === 'database.internal, database_2')));
    peer.write(encode({ channel: 40, kind: KIND.event, payload: invocation(requests, 'Connect') }));
    await until(() => calls.includes('network_connect'));
    assert.deepEqual(requests.find((request) => request.call === 'network_connect').with, {
      reference: networkId, container: containerId, aliases: ['database.internal', 'database_2'],
    });
    peer.write(encode({ channel: 41, kind: KIND.event, payload: invocation(requests, 'Disconnect') }));
    await until(() => requests.some((request) => request.call === 'interface_render_at'
      && request.with.frame.patches.some((patch) => patch.SetProp?.value?.Text === `Disconnect immutable container ${containerId} from network ${networkId}?`)));
    peer.write(encode({ channel: 42, kind: KIND.event, payload: invocation(requests, 'Confirm disconnect') }));
    await until(() => calls.includes('network_disconnect'));
    assert.deepEqual(requests.find((request) => request.call === 'network_disconnect').with, {
      reference: networkId, container: containerId,
    });
    peer.write(encode({ channel: 20, kind: KIND.event, payload: invocation(requests, 'Inspect') }));
    await until(() => calls.includes('network_inspect') && requests.some((request) => request.call === 'source_resize_at'
      && request.with.mutation.Length?.source === 204));
    const networkResize = requests.findLast((request) => request.call === 'source_resize_at'
      && request.with.mutation.Length?.source === 204);
    assert.deepEqual(networkResize.with.mutation.Length, { source: 204, version: 1, rows: 4 });
    peer.write(encode({ channel: 21, kind: KIND.event, payload: invocation(requests, 'Volumes') }));
    await until(() => requests.some((request) => request.call === 'interface_render_at'
      && request.with.frame.patches.some((patch) => patch.SetProp?.value?.Text === 'Volume name')));
    peer.write(encode({ channel: 43, kind: KIND.event, payload: changeInvocation(requests, 'Volume name', 'socket-cache') }));
    await until(() => requests.some((request) => request.call === 'interface_render_at'
      && request.with.frame.patches.some((patch) => patch.SetProp?.value?.Text === 'socket-cache')));
    peer.write(encode({ channel: 44, kind: KIND.event, payload: invocation(requests, 'Create') }));
    await until(() => calls.includes('volume_create') && requests.some((request) => request.call === 'interface_render_at'
      && request.with.frame.patches.some((patch) => patch.SetProp?.value?.Text === 'Created volume socket-cache.')));
    assert.deepEqual(requests.find((request) => request.call === 'volume_create').with, { name: 'socket-cache' });
    peer.write(encode({ channel: 22, kind: KIND.event, payload: invocation(requests, 'Inspect') }));
    await until(() => calls.includes('volume_inspect') && requests.some((request) => request.call === 'source_resize_at'
      && request.with.mutation.Length?.source === 205));
    const volumeResize = requests.findLast((request) => request.call === 'source_resize_at'
      && request.with.mutation.Length?.source === 205);
    assert.deepEqual(volumeResize.with.mutation.Length, { source: 205, version: 1, rows: 2 });
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

function changeInvocation(requests, placeholder, value) {
  const patches = requests.filter((request) => request.call === 'interface_render_at').flatMap((request) => request.with.frame.patches);
  const node = patches.findLast((patch) => patch.SetProp?.prop === 'Placeholder' && patch.SetProp.value?.Text === placeholder)?.SetProp.id;
  const handler = patches.findLast((patch) => patch.SetHandler?.id === node && patch.SetHandler.handler?.trigger === 'Change');
  assert.ok(handler, `${placeholder} advertises Change`);
  return { slot: 'workspace-resources', event: 'Change', node, id: handler.SetHandler.handler.id, value };
}

async function until(done) {
  const deadline = Date.now() + 5_000;
  while (!done()) {
    if (Date.now() >= deadline) throw new Error('entrypoint did not reach the host calls');
    await new Promise((resolve) => setTimeout(resolve, 20));
  }
}

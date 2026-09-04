import assert from 'node:assert/strict';
import { mkdtemp, rm } from 'node:fs/promises';
import net from 'node:net';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { spawn } from 'node:child_process';
import test from 'node:test';
import { KIND, Reader, encode } from '../../../packages/react/src/wire.js';

test('the production entrypoint handshakes and renders through a real Unix socket', { timeout: 8_000 }, async () => {
  const directory = await mkdtemp(join(tmpdir(), 'husklet-workspace-manager-'));
  const socketPath = join(directory, 'host.sock');
  const calls = [];
  const requests = [];
  const received = [];
  let peer;
  let executed = false;
  let containerInspectAttempts = 0;
  let imageInspectAttempts = 0;
  const containerId = 'a'.repeat(32);
  const executionId = 'c'.repeat(32);
  const createdContainerId = 'd'.repeat(32);
  const networkId = 'b'.repeat(32);
  const server = net.createServer((socket) => {
    peer = socket;
    const reader = new Reader();
    socket.on('error', (error) => {
      // A child closing its Unix socket may race the peer's final read and be
      // reported as ECONNRESET before Node delivers the child's close event.
      // The workflow assertions below still detect any premature disconnect.
      assert.equal(error.code, 'ECONNRESET');
    });
    socket.write(encode({ channel: 0, kind: KIND.open, payload: {
      protocol: 1, extension: 'workspace-manager', granted: ['containers:read', 'containers:control', 'containers:attach', 'images:read', 'images:write', 'volumes:read', 'volumes:write', 'networks:read', 'networks:write', 'interface:render'],
    } }));
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        received.push(frame);
        const name = frame.payload?.call;
        if (!name) continue;
        calls.push(name);
        requests.push(frame.payload);
        const inspectAttempt = name === 'container_inspect' ? ++containerInspectAttempts : 0;
        const imageInspectAttempt = name === 'image_inspect' ? ++imageInspectAttempts : 0;
        const payload = name === 'interface_open_tab'
          ? { reply: 'identity', with: 'workspace-resources' }
          : name === 'container_list'
          ? { reply: 'containers', with: [{ id: containerId, name: 'api', image: 'alpine:3.20', state: 'running', created: 0 }] }
          : name === 'container_inspect'
            ? inspectAttempt === 1
              ? { error: 'failed', detail: 'container inspect unavailable' }
              : { reply: 'container', with: { id: containerId, name: 'api', image: 'alpine:3.20', state: 'running', created: 0 } }
          : name === 'container_create'
            ? { reply: 'identity', with: createdContainerId }
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
              ? imageInspectAttempt === 2
                ? { error: 'failed', detail: 'image inspect unavailable' }
                : { reply: 'image_details', with: { id: 'i1', references: ['alpine:3.20'], created: 'now', size: 7, os: 'linux', architecture: 'amd64', entrypoint: ['/bin/sh'], command: [], working_directory: '/', user: '' } }
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
        const response = encode({ channel: frame.channel, kind: KIND.response, flags: inspectAttempt === 1 || imageInspectAttempt === 2 ? 3 : 1, payload });
        if (name === 'container_inspect' || name === 'image_inspect') setTimeout(() => socket.write(response), 20);
        else socket.write(response);
      }
    });
  });
  await new Promise((resolve, reject) => { server.once('error', reject); server.listen(socketPath, resolve); });
  const child = spawn(process.execPath, ['dist/main.js'], {
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
    await until(() => requests.some((request) => request.call === 'interface_render_at'
      && request.with.frame.patches.some((patch) => patch.SetProp?.value?.Text === 'Reading image details…')));
    await until(() => imageInspectAttempts === 1 && requests.some((request) => request.call === 'source_resize_at'
      && request.with.mutation.Length?.source === 201 && request.with.mutation.Length.rows === 9));
    await until(() => requests.some((request) => request.call === 'interface_render_at'
      && request.with.frame.patches.some((patch) => patch.SetProp?.value?.Text === '$.id')));
    const beforeRefresh = requests.length;
    peer.write(encode({ channel: 41, kind: KIND.event, payload: invocation(requests, 'Inspect') }));
    await until(() => requests.slice(beforeRefresh).some((request) => request.call === 'interface_render_at'
      && request.with.frame.patches.some((patch) => patch.Create?.tag === 'Progress')));
    await until(() => requests.some((request) => request.call === 'interface_render_at'
      && request.with.frame.patches.some((patch) => patch.SetProp?.value?.Text === 'image inspect unavailable')));
    assert(requests.slice(beforeRefresh).some((request) => request.call === 'interface_render_at'
      && request.with.frame.patches.some((patch) => patch.Remove)), 'loading removes stale ready detail before failure');
    peer.write(encode({ channel: 42, kind: KIND.event, payload: invocation(requests, 'Retry inspect') }));
    await until(() => imageInspectAttempts === 3 && requests.some((request) => request.call === 'source_resize_at'
      && request.with.mutation.Length?.source === 201 && request.with.mutation.Length.version === 2
      && request.with.mutation.Length.rows === 9));
    const resize = requests.findLast((request) => request.call === 'source_resize_at');
    assert.deepEqual(resize.with.mutation.Length, { source: 201, version: 2, rows: 9 });
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
    peer.write(encode({ channel: 51, kind: KIND.event, payload: changeInvocation(requests, 'Hostname (optional)', 'worker-1.internal') }));
    peer.write(encode({ channel: 52, kind: KIND.event, payload: changeInvocation(requests, 'Run as user (optional)', '1000:1000') }));
    peer.write(encode({ channel: 53, kind: KIND.event, payload: changeInvocation(requests, 'Labels JSON (optional)', '[["role","worker"],["tier","backend"]]') }));
    peer.write(encode({ channel: 54, kind: KIND.event, payload: changeInvocation(requests, 'Entrypoint argv JSON (optional)', '["/bin/sh","-lc"]') }));
    peer.write(encode({ channel: 55, kind: KIND.event, payload: changeInvocation(requests, 'Initial network (optional)', 'private_backend.v1') }));
    peer.write(encode({ channel: 43, kind: KIND.event, payload: changeInvocation(requests, 'Command argv JSON (optional)', '["sh","-lc","printf ready"]') }));
    peer.write(encode({ channel: 44, kind: KIND.event, payload: changeInvocation(requests, 'Environment pairs JSON (optional)', '[["MODE","test"],["EMPTY",""]]') }));
    peer.write(encode({ channel: 45, kind: KIND.event, payload: changeInvocation(requests, 'Working directory (optional)', '/workspace/app') }));
    peer.write(encode({ channel: 46, kind: KIND.event, payload: changeInvocation(requests, 'Memory limit MiB (optional)', '512') }));
    peer.write(encode({ channel: 47, kind: KIND.event, payload: changeInvocation(requests, 'CPU limit (optional)', '2') }));
    peer.write(encode({ channel: 48, kind: KIND.event, payload: changeInvocation(requests, 'PID limit (optional)', '128') }));
    peer.write(encode({ channel: 49, kind: KIND.event, payload: changeInvocation(requests, 'Named volume mounts JSON (optional)', '[{"volume":"cache","target":"/cache","read_only":true},{"volume":"data","target":"/srv/data"}]') }));
    peer.write(encode({ channel: 50, kind: KIND.event, payload: changeInvocation(requests, 'Published ports JSON (optional)', '[{"container":8080,"host":18080,"protocol":"tcp"},{"container":53,"protocol":"udp"}]') }));
    await until(() => requests.some((request) => request.call === 'interface_render_at'
      && request.with.frame.patches.some((patch) => patch.SetProp?.value?.Text === 'worker')));
    peer.write(encode({ channel: 28, kind: KIND.event, payload: invocation(requests, 'Create and start') }));
    await until(() => calls.includes('container_create') && calls.includes('container_start'));
    assert.deepEqual(requests.find((request) => request.call === 'container_create').with.spec, {
      image: 'alpine:3.20', name: 'worker', entrypoint: ['/bin/sh', '-lc'], command: ['sh', '-lc', 'printf ready'], environment: [['MODE', 'test'], ['EMPTY', '']], working_directory: '/workspace/app',
      hostname: 'worker-1.internal', user: '1000:1000', labels: [['role', 'worker'], ['tier', 'backend']], mounts: [
        { volume: 'cache', target: '/cache', read_only: true },
        { volume: 'data', target: '/srv/data', read_only: false },
      ], network: 'private_backend.v1', ports: [
        { container: 8080, host: 18080, protocol: 'tcp' },
        { container: 53, host: null, protocol: 'udp' },
      ], memory_mb: 512, cpus: 2, pids_limit: 128,
    });
    assert.deepEqual(requests.find((request) => request.call === 'container_start').with, { id: createdContainerId });
    peer.write(encode({ channel: 12, kind: KIND.event, payload: invocation(requests, 'Details') }));
    await until(() => requests.some((request) => request.call === 'interface_render_at'
      && request.with.frame.patches.some((patch) => patch.SetProp?.value?.Text === 'Reading container details…')));
    await until(() => requests.some((request) => request.call === 'interface_render_at'
      && request.with.frame.patches.some((patch) => patch.SetProp?.value?.Text === 'container inspect unavailable')));
    peer.write(encode({ channel: 38, kind: KIND.event, payload: invocation(requests, 'Retry details') }));
    await until(() => containerInspectAttempts === 2 && requests.some((request) =>
      request.call === 'source_resize_at' && request.with.mutation.Length?.source === 202
      && request.with.mutation.Length.version === 1));
    peer.write(encode({ channel: 39, kind: KIND.event, payload: invocation(requests, 'Hide details') }));
    await until(() => requests.findLast((request) => request.call === 'interface_render_at')?.with.frame.patches
      .some((patch) => patch.SetProp?.value?.Text === 'Details'));
    peer.write(encode({ channel: 40, kind: KIND.event, payload: invocation(requests, 'Details') }));
    await until(() => containerInspectAttempts === 3 && requests.some((request) =>
      request.call === 'source_resize_at' && request.with.mutation.Length?.source === 202
      && request.with.mutation.Length.rows === 5));
    const containerResize = requests.findLast((request) => request.call === 'source_resize_at'
      && request.with.mutation.Length?.source === 202);
    assert.deepEqual(containerResize.with.mutation.Length, { source: 202, version: 1, rows: 5 });
    await barrier(peer, received, 'container-details-ready');
    const execute = invocation(requests, 'Execute');
    const changes = [
      [29, 'Command argv JSON', '["sh","-lc","printf hello world"]'],
      [32, 'Run as user (optional)', '1000:1000'],
      [33, 'Working directory (optional)', '/work tree'],
    ];
    peer.write(Buffer.concat(changes.map(([channel, placeholder, value]) =>
      encode({ channel, kind: KIND.event, payload: changeInvocation(requests, placeholder, value) }))));
    await until(() => changes.every(([, , value]) => requests.some((request) => request.call === 'interface_render_at'
      && request.with.frame.patches.some((patch) => patch.SetProp?.value?.Text === value))));
    // Seeing the render call precedes the extension receiving its reply. A
    // control-channel ping is an ordered barrier proving it consumed that reply.
    await barrier(peer, received, 'execution-form-ready');
    peer.write(encode({ channel: 30, kind: KIND.event, payload: execute }));
    try {
      await until(() => calls.includes('container_exec') && requests.some((request) => request.call === 'interface_render_at'
        && request.with.frame.patches.some((patch) => patch.SetProp?.value?.Text === `Execution ${executionId} created.`)));
    } catch (error) {
      throw new Error(`${error.message}; tail=${JSON.stringify(calls.slice(-12))}; stderr=${JSON.stringify(stderr)}`);
    }
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
    const closed = child.exitCode === null && child.signalCode === null
      ? new Promise((resolve) => child.once('close', resolve))
      : Promise.resolve();
    child.kill('SIGTERM');
    await closed;
    peer?.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

function invocation(requests, label) {
  const patches = requests.filter((request) => request.call === 'interface_render_at')
    .flatMap((request) => request.with.frame.patches);
  const active = activeNodes(patches);
  const labelled = patches.filter((patch) => patch.SetProp?.prop === 'Label' && patch.SetProp.value?.Text === label);
  assert.ok(labelled.length, `${label} is present on the live socket surface`);
  const handler = labelled.toReversed().map((candidate) => {
    const node = candidate.SetProp.id;
    const installed = patches.findLastIndex((patch) => patch.SetHandler?.handler?.trigger === 'Invoke'
      && patch.SetHandler.id === node);
    return installed >= 0 && active(node) ? patches[installed] : undefined;
  }).find(Boolean);
  assert.ok(handler, `${label} advertises Invoke`);
  return { slot: 'workspace-resources', event: 'Invoke', node: handler.SetHandler.id, id: handler.SetHandler.handler.id };
}

function changeInvocation(requests, placeholder, value) {
  const patches = requests.filter((request) => request.call === 'interface_render_at').flatMap((request) => request.with.frame.patches);
  const active = activeNodes(patches);
  const node = patches.filter((patch) => patch.SetProp?.prop === 'Placeholder' && patch.SetProp.value?.Text === placeholder)
    .toReversed().find((patch) => active(patch.SetProp.id))?.SetProp.id;
  const handler = patches.findLast((patch) => patch.SetHandler?.id === node && patch.SetHandler.handler?.trigger === 'Change');
  assert.ok(handler, `${placeholder} advertises Change`);
  return { slot: 'workspace-resources', event: 'Change', node, id: handler.SetHandler.handler.id, value };
}

function activeNodes(patches) {
  const parents = new Map();
  const removed = new Set();
  for (const patch of patches) {
    if (patch.Insert) { parents.set(patch.Insert.child, patch.Insert.parent); removed.delete(patch.Insert.child); }
    if (patch.Remove) removed.add(patch.Remove.id);
  }
  return (node) => {
    for (let current = node; current !== undefined; current = parents.get(current)) {
      if (removed.has(current)) return false;
    }
    return true;
  };
}

async function barrier(peer, received, token) {
  const payload = Buffer.from(token);
  peer.write(encode({ channel: 0, kind: KIND.ping, payload }));
  await until(() => received.some((frame) => frame.channel === 0 && frame.kind === KIND.pong && frame.payload.equals(payload)));
}

async function until(done) {
  const deadline = Date.now() + 5_000;
  while (!done()) {
    if (Date.now() >= deadline) throw new Error('entrypoint did not reach the host calls');
    await new Promise((resolve) => setTimeout(resolve, 20));
  }
}

import assert from 'node:assert/strict';
import fs from 'node:fs';
import net from 'node:net';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { Client } from '@modelcontextprotocol/sdk/client/index.js';
import { StdioClientTransport } from '@modelcontextprotocol/sdk/client/stdio.js';
import { CONTROL, KIND, Reader, encode } from '../../react/src/wire.js';
import { runAgentAdmin } from '../examples/agent-admin.mjs';

const workspaceConfiguration = {
  name: 'managed', image: 'alpine:3.20', architecture: 'amd64', storage: null, shell: null,
  cpus: 1, memory_mb: 512, environment: [], mounts: [], docker_socket: false,
  scrollback: null, vpn: null, execution_lifetime: 'ephemeral', terminal: {
    font_family: null, font_size: null, foreground: null, background: null,
    cursor_shape: null, cursor_blink: null,
  },
};

test('admin workflow confines files to socket workspace and cleans success and failure over real framing', async (context) => {
  const scratch = fs.mkdtempSync(path.join(os.tmpdir(), 'husklet-admin-'));
  const socketPath = path.join(scratch, 'host.sock');
  const calls = [];
  const sockets = new Set();
  let failRead = false;
  let channel = 60;
  let revision = 0;
  const host = net.createServer((socket) => {
    sockets.add(socket); socket.once('close', () => sockets.delete(socket));
    const reader = new Reader();
    socket.write(encode({ channel: CONTROL, kind: KIND.request, payload: {
      protocol: 1, extension: 'admin', peer: 'admin', granted: [],
    } }));
    const answer = (frame, reply, withValue) => socket.write(encode({
      channel: frame.channel, kind: KIND.response,
      payload: withValue === undefined ? { reply } : { reply, with: withValue },
    }));
    const lifecycle = (workspace, action) => socket.write(encode({ channel: channel++, kind: KIND.event, payload: {
      snapshot: 'workspace_lifecycle', of: { workspace, action, revision: ++revision, coalesced: 0 },
    } }));
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.channel === CONTROL || frame.kind !== KIND.request) continue;
        calls.push(frame.payload);
        const { call, with: argument } = frame.payload;
        if (call === 'workspace_info') answer(frame, 'workspace', { name: 'observer' });
        else if (call === 'workspace_create') {
          answer(frame, 'workspace_configuration', { ...argument.configuration, generation: '0123456789abcdef0123456789abcdef' });
          if (revision === 0) lifecycle('another-workspace', 'create');
          lifecycle(argument.configuration.name, 'create');
        }
        else if (call === 'container_create') answer(frame, 'identity', 'c'.repeat(64));
        else if (['workspace_start', 'workspace_stop', 'workspace_delete', 'execution_kill', 'filesystem_mkdir', 'filesystem_write', 'filesystem_remove', 'event_subscribe', 'event_unsubscribe', 'terminal_spawn', 'terminal_write_pane'].includes(call)) {
          answer(frame, 'done');
          if (call === 'workspace_start') lifecycle(argument.name, 'start');
          if (call === 'workspace_stop') lifecycle(argument.name, 'stop');
          if (call === 'workspace_delete') lifecycle(argument.name, 'remove');
          if (call === 'terminal_write_pane') socket.write(encode({ channel: channel++, kind: KIND.event, payload: {
            snapshot: 'pane_changes', of: { slot: 'admin-terminal', kind: 'terminal', revision: 2, generation: 3, coalesced: 0 },
          } }));
        } else if (call === 'filesystem_read' && failRead) {
          failRead = false;
          socket.write(encode({ channel: frame.channel, kind: KIND.response, flags: 3, payload: {
            error: 'failed', call, detail: 'fixture read failure',
          } }));
        } else if (call === 'filesystem_read') answer(frame, 'contents', 'hello admin');
        else throw new Error(`unexpected call ${call}`);
      }
    });
  });
  await new Promise((resolve, reject) => host.listen(socketPath, resolve).once('error', reject));
  context.after(async () => {
    for (const socket of sockets) socket.destroy();
    await new Promise((resolve) => host.close(resolve));
    fs.rmSync(scratch, { recursive: true, force: true });
  });
  const transport = new StdioClientTransport({
    command: process.execPath,
    args: [path.resolve(import.meta.dirname, '../src/cli.js'), '--socket', socketPath, '--workspace', 'observer'],
    stderr: 'pipe',
  });
  let diagnostics = ''; transport.stderr.on('data', (chunk) => { diagnostics += chunk; });
  const client = new Client({ name: 'admin-test', version: '1' });
  await client.connect(transport);
  const options = {
    hostingWorkspace: 'observer', workspaceConfiguration,
    directory: 'agent-admin', file: 'agent-admin/note.txt', contents: 'hello admin',
    eventSlot: 'admin-terminal', eventInput: 'status\n', waitMs: 1_000,
  };
  const result = await runAgentAdmin(client, options);
  assert.equal(result.read, 'hello admin');
  assert.equal(result.event.changed, true);
  assert.deepEqual(result.lifecycle.map(({ change }) => [change.workspace, change.action]), [
    ['managed', 'create'], ['managed', 'start'], ['managed', 'stop'], ['managed', 'remove'],
  ]);

  const beforeMismatch = calls.length;
  await assert.rejects(() => runAgentAdmin(client, { ...options, hostingWorkspace: 'managed' }), /does not match socket workspace/);
  assert.deepEqual(calls.slice(beforeMismatch).map(({ call }) => call), ['workspace_info']);

  failRead = true;
  const failureStart = calls.length;
  await assert.rejects(() => runAgentAdmin(client, options), /husklet_file_read failed: fixture read failure/);
  assert.deepEqual(calls.slice(failureStart).map(({ call }) => call), [
    'workspace_info', 'event_subscribe', 'workspace_create', 'event_unsubscribe',
    'event_subscribe', 'workspace_start', 'event_unsubscribe', 'filesystem_mkdir', 'filesystem_write',
    'filesystem_read', 'filesystem_remove', 'filesystem_remove', 'event_subscribe', 'workspace_stop',
    'event_unsubscribe', 'event_subscribe', 'workspace_delete', 'event_unsubscribe',
  ]);

  const exactContents = 'é'.repeat(32_768);
  const exactStart = calls.length;
  const exact = await client.callTool({ name: 'husklet_file_write', arguments: { path: 'exact.txt', contents: exactContents } });
  assert.notEqual(exact.isError, true);
  assert.deepEqual(calls.slice(exactStart), [{
    call: 'filesystem_write',
    with: { path: 'exact.txt', contents: [...new TextEncoder().encode(exactContents)] },
  }]);

  const oversizedStart = calls.length;
  const oversized = await client.callTool({
    name: 'husklet_file_write',
    arguments: { path: 'oversized.txt', contents: '😀'.repeat(16_385) },
  });
  assert.equal(oversized.isError, true);
  assert.equal(calls.length, oversizedStart);

  const exactPath = 'é'.repeat(2048);
  const pathExactStart = calls.length;
  const pathExact = await client.callTool({ name: 'husklet_file_read', arguments: { path: exactPath } });
  assert.notEqual(pathExact.isError, true);
  assert.deepEqual(calls.slice(pathExactStart), [{ call: 'filesystem_read', with: { path: exactPath } }]);

  const pathOversizedStart = calls.length;
  const pathOversized = await client.callTool({
    name: 'husklet_file_read', arguments: { path: '😀'.repeat(1025) },
  });
  assert.equal(pathOversized.isError, true);
  assert.equal(calls.length, pathOversizedStart);

  const exactImage = 'é'.repeat(256);
  const imageExactStart = calls.length;
  const imageExact = await client.callTool({
    name: 'husklet_container_create', arguments: { image: exactImage, name: 'wire-boundary' },
  });
  assert.notEqual(imageExact.isError, true);
  assert.equal(calls.length, imageExactStart + 1);
  assert.equal(calls[imageExactStart].call, 'container_create');
  assert.equal(calls[imageExactStart].with.spec.image, exactImage);

  const imageOversizedStart = calls.length;
  const imageOversized = await client.callTool({
    name: 'husklet_container_create',
    arguments: { image: '😀'.repeat(129), name: 'wire-overflow' },
  });
  assert.equal(imageOversized.isError, true);
  assert.equal(calls.length, imageOversizedStart);

  const exactUser = 'é'.repeat(128);
  const userExactStart = calls.length;
  const userExact = await client.callTool({
    name: 'husklet_container_create',
    arguments: { image: 'alpine:3.20', name: 'user-boundary', user: exactUser },
  });
  assert.notEqual(userExact.isError, true);
  assert.equal(calls.length, userExactStart + 1);
  assert.equal(calls[userExactStart].call, 'container_create');
  assert.equal(calls[userExactStart].with.spec.user, exactUser);

  const userOversizedStart = calls.length;
  const userOversized = await client.callTool({
    name: 'husklet_container_create',
    arguments: { image: 'alpine:3.20', name: 'user-overflow', user: '😀'.repeat(65) },
  });
  assert.equal(userOversized.isError, true);
  assert.equal(calls.length, userOversizedStart);

  const exactEnvironment = 'é'.repeat(4096);
  const environmentExactStart = calls.length;
  const environmentExact = await client.callTool({
    name: 'husklet_container_create',
    arguments: { image: 'alpine:3.20', name: 'environment-boundary', environment: [['VALUE', exactEnvironment]] },
  });
  assert.notEqual(environmentExact.isError, true);
  assert.equal(calls.length, environmentExactStart + 1);
  assert.equal(calls[environmentExactStart].call, 'container_create');
  assert.deepEqual(calls[environmentExactStart].with.spec.environment, [['VALUE', exactEnvironment]]);

  const environmentOversizedStart = calls.length;
  const environmentOversized = await client.callTool({
    name: 'husklet_container_create',
    arguments: { image: 'alpine:3.20', name: 'environment-overflow', environment: [['VALUE', '😀'.repeat(2049)]] },
  });
  assert.equal(environmentOversized.isError, true);
  assert.equal(calls.length, environmentOversizedStart);

  const exactLabel = 'é'.repeat(2048);
  const labelExactStart = calls.length;
  const labelExact = await client.callTool({
    name: 'husklet_container_create',
    arguments: { image: 'alpine:3.20', name: 'label-boundary', labels: [['note', exactLabel]] },
  });
  assert.notEqual(labelExact.isError, true);
  assert.equal(calls.length, labelExactStart + 1);
  assert.equal(calls[labelExactStart].call, 'container_create');
  assert.deepEqual(calls[labelExactStart].with.spec.labels, [['note', exactLabel]]);

  const labelOversizedStart = calls.length;
  const labelOversized = await client.callTool({
    name: 'husklet_container_create',
    arguments: { image: 'alpine:3.20', name: 'label-overflow', labels: [['note', '😀'.repeat(1025)]] },
  });
  assert.equal(labelOversized.isError, true);
  assert.equal(calls.length, labelOversizedStart);

  const exactLabelName = 'é'.repeat(128);
  const labelNameStart = calls.length;
  const labelNameExact = await client.callTool({
    name: 'husklet_container_create',
    arguments: { image: 'alpine:3.20', name: 'label-name-boundary', labels: [[exactLabelName, 'note']] },
  });
  assert.notEqual(labelNameExact.isError, true);
  assert.equal(calls.length, labelNameStart + 1);
  assert.equal(calls[labelNameStart].call, 'container_create');
  assert.deepEqual(calls[labelNameStart].with.spec.labels, [[exactLabelName, 'note']]);

  const labelNameOversizedStart = calls.length;
  const labelNameOversized = await client.callTool({
    name: 'husklet_container_create',
    arguments: { image: 'alpine:3.20', name: 'label-name-overflow', labels: [['😀'.repeat(65), 'note']] },
  });
  assert.equal(labelNameOversized.isError, true);
  assert.equal(calls.length, labelNameOversizedStart);

  const exactMountTarget = `/${'é'.repeat(2047)}a`;
  const mountStart = calls.length;
  const mountExact = await client.callTool({
    name: 'husklet_container_create',
    arguments: { image: 'alpine:3.20', name: 'mount-boundary', mounts: [{ volume: 'cache', target: exactMountTarget }] },
  });
  assert.notEqual(mountExact.isError, true);
  assert.equal(calls.length, mountStart + 1);
  assert.deepEqual(calls[mountStart].with.spec.mounts, [{ volume: 'cache', target: exactMountTarget, read_only: false }]);

  const mountOversizedStart = calls.length;
  const mountOversized = await client.callTool({
    name: 'husklet_container_create',
    arguments: { image: 'alpine:3.20', name: 'mount-overflow', mounts: [{ volume: 'cache', target: `/${'😀'.repeat(1024)}a` }] },
  });
  assert.equal(mountOversized.isError, true);
  assert.equal(calls.length, mountOversizedStart);

  const exactCommand = ['printf', '%s', '$(touch /tmp/not-run)', 'two words', "single'quote", ''];
  const spawnStart = calls.length;
  const spawned = await client.callTool({
    name: 'husklet_terminal_spawn', arguments: { slot: 'admin-terminal', command: exactCommand },
  });
  assert.notEqual(spawned.isError, true);
  assert.deepEqual(calls.slice(spawnStart), [{
    call: 'terminal_spawn', with: { slot: 'admin-terminal', command: exactCommand },
  }]);

  const exactInput = 'é'.repeat(32_768);
  const inputStart = calls.length;
  const written = await client.callTool({
    name: 'husklet_terminal_write', arguments: { slot: 'admin-terminal', input: exactInput },
  });
  assert.notEqual(written.isError, true);
  assert.equal(calls.length, inputStart + 1);
  assert.equal(calls[inputStart].call, 'terminal_write_pane');
  assert.equal(calls[inputStart].with.slot, 'admin-terminal');
  assert.deepEqual(
    Uint8Array.from(calls[inputStart].with.contents),
    new TextEncoder().encode(exactInput),
  );

  const inputOversizedStart = calls.length;
  const inputOversized = await client.callTool({
    name: 'husklet_terminal_write',
    arguments: { slot: 'admin-terminal', input: '😀'.repeat(16_385) },
  });
  assert.equal(inputOversized.isError, true);
  assert.equal(calls.length, inputOversizedStart);

  const executionId = 'e'.repeat(32);
  const signalStart = calls.length;
  const signaled = await client.callTool({
    name: 'husklet_execution_signal', arguments: { id: executionId, signal: 'SIGRTMAX-14' },
  });
  assert.notEqual(signaled.isError, true);
  assert.deepEqual(calls.slice(signalStart), [{
    call: 'execution_kill', with: { id: executionId, signal: 'SIGRTMAX-14' },
  }]);

  const signalOversizedStart = calls.length;
  const signalOversized = await client.callTool({
    name: 'husklet_execution_signal', arguments: { id: executionId, signal: '😀'.repeat(9) },
  });
  assert.equal(signalOversized.isError, true);
  assert.equal(calls.length, signalOversizedStart);
  await client.close();
  assert.equal(diagnostics, '');

  assert.deepEqual(calls.slice(0, beforeMismatch).map(({ call }) => call), [
    'workspace_info', 'workspace_info', 'event_subscribe', 'workspace_create', 'event_unsubscribe',
    'event_subscribe', 'workspace_start', 'event_unsubscribe', 'filesystem_mkdir',
    'filesystem_write', 'filesystem_read', 'event_subscribe', 'terminal_write_pane', 'event_unsubscribe',
    'filesystem_remove', 'filesystem_remove', 'event_subscribe', 'workspace_stop', 'event_unsubscribe',
    'event_subscribe', 'workspace_delete', 'event_unsubscribe',
  ]);
  assert.deepEqual(calls.find(({ call }) => call === 'filesystem_write').with, {
    path: 'agent-admin/note.txt', contents: [...new TextEncoder().encode('hello admin')],
  });
  assert.deepEqual(calls.filter(({ call }) => call === 'filesystem_remove').slice(0, 2).map(({ with: value }) => value), [
    { path: 'agent-admin/note.txt' }, { path: 'agent-admin' },
  ]);
});

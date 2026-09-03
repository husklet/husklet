import assert from 'node:assert/strict';
import fs from 'node:fs';
import net from 'node:net';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { Client } from '@modelcontextprotocol/sdk/client/index.js';
import { StdioClientTransport } from '@modelcontextprotocol/sdk/client/stdio.js';
import { CONTROL, KIND, Reader, encode } from '../../react/src/wire.js';
import { runAgentDayOne, waitForExecutionRemoval, waitForInstalledExtensionChange } from '../examples/agent-day-one.mjs';

const configuration = (image) => ({
  name: 'target', image, architecture: 'amd64', storage: null, shell: null, cpus: 2, memory_mb: 1024,
  environment: [], mounts: [], docker_socket: false, scrollback: null, vpn: null,
  execution_lifetime: 'persisted', terminal: {
    font_family: null, font_size: null, foreground: null, background: null,
    cursor_shape: null, cursor_blink: null,
  },
});

test('day-one agent drives exact framed host requests and confirmed cleanup through spawned MCP', async (context) => {
  const containerId = 'a'.repeat(64);
  const scratch = fs.mkdtempSync(path.join(os.tmpdir(), 'husklet-day-one-'));
  const socketPath = path.join(scratch, 'host.sock');
  const original = { ...configuration('alpine:3.20'), generation: '0123456789abcdef0123456789abcdef' };
  const updated = configuration('alpine:3.21');
  const calls = [];
  const credits = [];
  const sockets = new Set();
  let eventChannel = 40;
  let executionSubscriptions = 0;
  const host = net.createServer((socket) => {
    sockets.add(socket); socket.once('close', () => sockets.delete(socket));
    const reader = new Reader();
    socket.write(encode({ channel: CONTROL, kind: KIND.request, payload: {
      protocol: 1, extension: 'agent', peer: 'agent', granted: [],
    } }));
    const answer = (frame, reply, withValue) => socket.write(encode({
      channel: frame.channel, kind: KIND.response,
      payload: withValue === undefined ? { reply } : { reply, with: withValue },
    }));
    const changed = (slot, kind, generation, revision = generation) => socket.write(encode({
      channel: eventChannel++, kind: KIND.event, payload: { snapshot: 'pane_changes', of: {
        slot, kind, revision, generation, coalesced: 0,
      } },
    }));
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.kind === KIND.credit) { credits.push(frame); continue; }
        if (frame.channel === CONTROL || frame.kind !== KIND.request) continue;
        calls.push(frame.payload);
        const { call, with: argument } = frame.payload;
        if (call === 'workspace_info') answer(frame, 'workspace', { name: 'observer' });
        else if (call === 'extension_list') answer(frame, 'extensions', [{ name: 'manager', image_digest: `sha256:${'a'.repeat(64)}`, status: 'standby' }]);
        else if (call === 'image_pull_start') answer(frame, 'image_pull_job', { job: '7' });
        else if (call === 'image_pull_status') answer(frame, 'image_pull', {
          job: argument.job, revision: calls.filter(({ call: name }) => name === 'image_pull_status').length,
          reference: 'alpine:3.21', state: calls.filter(({ call: name }) => name === 'image_pull_status').length > 1 ? 'complete' : 'pulling',
          layers: [], current: 1, total: 1, error: null,
        });
        else if (call === 'workspace_inspect') answer(frame, 'workspace_configuration', original);
        else if (call === 'workspace_update') answer(frame, 'workspace_configuration', argument.configuration);
        else if (call === 'container_create') answer(frame, 'identity', containerId);
        else if (call === 'container_start' || call === 'container_stop' || call === 'container_remove'
          || call === 'terminal_write_pane' || call === 'pane_semantic_action'
          || call === 'event_subscribe' || call === 'event_unsubscribe') {
          answer(frame, 'done');
          if (call === 'event_subscribe' && argument.topic === 'image-pulls') setImmediate(() => socket.write(encode({
            channel: eventChannel++, kind: KIND.event, payload: { snapshot: 'image_pulls', of: {
              job: '7', revision: 2, state: 'complete', coalesced: 0,
            } },
          })));
          if (call === 'event_subscribe' && argument.topic === 'containers') setImmediate(() => {
            socket.write(encode({ channel: eventChannel++, kind: KIND.event, payload: { snapshot: 'containers', of: [
              { id: containerId, name: 'day-one', image: 'alpine:3.21', state: 'running', created: 41 },
            ] } }));
            setImmediate(() => socket.write(encode({ channel: eventChannel++, kind: KIND.event, payload: { snapshot: 'containers', of: [
              { id: containerId, name: 'day-one', image: 'alpine:3.21', state: 'exited', created: 41 },
            ] } })));
          });
          if (call === 'event_subscribe' && argument.topic === 'executions') setImmediate(() => {
            executionSubscriptions += 1;
            if (executionSubscriptions === 1) {
              const removed = { id: 'execution-remove', container_id: containerId, running: false, exit_code: 0, pid: 0, command: ['/bin/remove'], user: 'app' };
              const transitioning = { id: 'execution-state', container_id: containerId, running: true, exit_code: 0, pid: 19, command: ['/bin/state'], user: 'app' };
              socket.write(encode({ channel: eventChannel++, kind: KIND.event, payload: { snapshot: 'executions', of: { executions: [removed, transitioning], truncated: false } } }));
              setImmediate(() => socket.write(encode({ channel: eventChannel++, kind: KIND.event, payload: { snapshot: 'executions', of: { executions: [{ ...transitioning, running: false, pid: 0 }], truncated: false } } })));
            } else {
              const summary = { id: 'execution-day-one', container_id: containerId, running: true, exit_code: 0, pid: 17, command: ['/usr/bin/worker', '--once'], user: 'app' };
              socket.write(encode({ channel: eventChannel++, kind: KIND.event, payload: { snapshot: 'executions', of: { executions: [summary], truncated: false } } }));
              setImmediate(() => socket.write(encode({ channel: eventChannel++, kind: KIND.event, payload: { snapshot: 'executions', of: { executions: [{ ...summary, running: false, exit_code: 0, pid: 0 }], truncated: false } } })));
            }
          });
          if (call === 'event_subscribe' && argument.topic === 'extensions') setImmediate(() => {
            const extension = { name: 'manager', image_digest: `sha256:${'a'.repeat(64)}`, status: 'standby' };
            socket.write(encode({ channel: eventChannel++, kind: KIND.event, payload: { snapshot: 'extensions', of: [extension] } }));
            setImmediate(() => socket.write(encode({ channel: eventChannel++, kind: KIND.event, payload: { snapshot: 'extensions', of: [{ ...extension, status: 'duty' }] } })));
          });
          if (call === 'event_subscribe' && argument.topic === 'pane-changes') setImmediate(() => {
            changed('terminal-1', 'terminal', 1, 0);
            changed('surface-1', 'surface', 7, 7);
          });
          if (call === 'terminal_write_pane') changed('terminal-1', 'terminal', 2);
          if (call === 'pane_semantic_action') changed('surface-1', 'surface', 8, 0);
        } else if (call === 'container_exec') answer(frame, 'identity', 'execution-day-one');
        else if (call === 'execution_inspect') answer(frame, 'execution', { id: argument.id, container_id: containerId, running: true, exit_code: 0, pid: 17, command: ['/usr/bin/worker', '--once'], user: 'app' });
        else if (call === 'container_processes') answer(frame, 'processes', [{ pid: 7, command: 'worker', user: 'app' }]);
        else if (call === 'pane_list') answer(frame, 'panes', { panes: [
          { slot: 'terminal-1', generation: 1, revision: 0, kind: 'terminal', provider: null, tab: 'tab-1', title: 'Shell', focused: true },
          { slot: 'surface-1', generation: 7, revision: 7, kind: 'surface', provider: { extension: 'manager', provider: 'main' }, tab: 'tab-1', title: 'Manager', focused: false },
        ], truncated: false });
        else if (call === 'terminal_topology') answer(frame, 'topology', { active_tab: 'tab-1', tabs: [{ id: 'tab-1', title: 'Day one', root: {
          kind: 'split', division: 'beside', ratio_per_mille: 500,
          first: { kind: 'pane', focused: true, grid: { columns: 80, rows: 24 }, pane: { slot: 'terminal-1', occupant: 'terminal', working_directory: '/work', command: 'sh', provider: null } },
          second: { kind: 'pane', focused: false, grid: null, pane: { slot: 'surface-1', occupant: 'surface', working_directory: null, command: null, provider: { extension: 'manager', provider: 'main' } } },
        } }] });
        else if (call === 'terminal_read_pane') answer(frame, 'text', { slot: argument.slot, lines: ['ready'], truncated: false });
        else if (call === 'pane_semantic_read') answer(frame, 'semantics', { slot: argument.slot, revision: 7, truncated: false, root: {
          id: 0, role: 'column', label: null, value: null, disabled: false, destructive: false, actions: [], children: [{
            id: 5, role: 'button', label: 'Refresh', value: null, disabled: false, destructive: false, actions: ['invoke'], children: [],
          }],
        } });
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
  const client = new Client({ name: 'day-one-test', version: '1' });
  await client.connect(transport);
  const extensionChanged = await waitForInstalledExtensionChange(client, 'manager', 1_000);
  assert.equal(extensionChanged.extension.status, 'duty');
  const removedExecution = { id: 'execution-remove', container_id: containerId, running: false, exit_code: 0, pid: 0, command: ['/bin/remove'], user: 'app' };
  const stateExecution = { id: 'execution-state', container_id: containerId, running: true, exit_code: 0, pid: 19, command: ['/bin/state'], user: 'app' };
  const [removed, transitioned] = await Promise.all([
    waitForExecutionRemoval(client, removedExecution, 1_000),
    client.callTool({ name: 'husklet_execution_change_wait', arguments: { id: stateExecution.id, after: {
      container_id: stateExecution.container_id, running: stateExecution.running, exit_code: stateExecution.exit_code,
      pid: stateExecution.pid, command: stateExecution.command, user: stateExecution.user,
    }, running: false, timeout_ms: 1_000 } }).then(({ content }) => JSON.parse(content[0].text)),
  ]);
  assert.equal(removed.removed, true); assert.equal(removed.execution, null);
  assert.equal(transitioned.execution.running, false);
  const containerChanged = await client.callTool({ name: 'husklet_container_change_wait', arguments: {
    id: containerId, after: { state: 'running', created: 41 }, timeout_ms: 1_000,
  } });
  assert.equal(JSON.parse(containerChanged.content[0].text).container.state, 'exited');
  const result = await runAgentDayOne(client, {
    workspaceName: 'target', updatedConfiguration: updated,
    container: { image: 'alpine:3.21', name: 'day-one', command: ['/usr/bin/worker', '--once'] },
    terminalInput: 'status\n', actionLabel: 'Refresh', waitMs: 1_000, pullImage: true,
  });
  assert.equal(result.container.imagePull.job, '7');
  assert.equal(result.container.imagePull.state, 'complete');
  assert.deepEqual(result.container.created, { id: containerId });
  assert.equal(result.container.execution.id, 'execution-day-one');
  assert.equal(result.container.executionChanged.execution.running, false);
  assert.equal(result.terminal.changed.changed, true);
  assert.equal(result.terminal.changed.change.generation, 2);
  assert.equal(result.terminal.changed.change.revision, 2);
  assert.equal(result.semantic.node, 5);
  assert.equal(result.semantic.changed.change.generation, 8);
  assert.equal(result.semantic.changed.change.revision, 0, 'a replacement generation may reset revision');
  await client.close();
  assert.equal(diagnostics, '');

  assert.deepEqual(calls.map(({ call }) => call), [
    'workspace_info', 'extension_list', 'event_subscribe', 'event_unsubscribe',
    'event_subscribe', 'event_unsubscribe', 'event_subscribe', 'event_unsubscribe',
    'workspace_inspect', 'image_pull_start', 'image_pull_status',
    'event_subscribe', 'image_pull_status', 'event_unsubscribe',
    'workspace_update', 'container_create', 'container_start',
    'container_exec', 'execution_inspect', 'event_subscribe', 'event_unsubscribe', 'container_processes', 'pane_list', 'pane_list', 'terminal_topology', 'terminal_read_pane',
    'event_subscribe', 'terminal_write_pane', 'event_unsubscribe', 'pane_semantic_read',
    'event_subscribe', 'pane_semantic_read', 'pane_semantic_action', 'event_unsubscribe',
    'pane_semantic_read', 'container_stop', 'container_remove', 'workspace_update',
  ]);
  assert.equal(credits.some(({ payload }) => payload === 1), true);
  assert.equal(calls.filter(({ call, with: value }) => call === 'event_subscribe' && value.topic === 'executions').length, 2,
    'two concurrent waits share one subscription, followed by the workflow subscription');
  assert.equal(calls.filter(({ call, with: value }) => call === 'event_unsubscribe' && value.topic === 'executions').length, 2,
    'the concurrent subscription remains until both waits dispose');
  assert.deepEqual(calls.find(({ call }) => call === 'container_exec').with, {
    id: containerId, command: ['/usr/bin/worker', '--once'], user: null, working_directory: null,
  });
  assert.deepEqual(calls.find(({ call }) => call === 'container_create').with.spec, {
    image: 'alpine:3.21', name: 'day-one', hostname: null, entrypoint: null,
    command: ['/usr/bin/worker', '--once'], environment: [], working_directory: null, user: null,
    labels: [['husklet.agent-workflow', 'day-one']], mounts: [], network: null, ports: [],
    memory_mb: 512, cpus: 1, pids_limit: 128,
  });
  assert.deepEqual(calls.find(({ call }) => call === 'terminal_write_pane').with, {
    slot: 'terminal-1', contents: [...new TextEncoder().encode('status\n')],
  });
  assert.deepEqual(calls.find(({ call }) => call === 'pane_semantic_action').with, {
    slot: 'surface-1', action: { revision: 7, node: 5, action: 'invoke' },
  });
  assert.deepEqual(calls.filter(({ call }) => call === 'workspace_update').map(({ with: value }) => value), [
    { name: 'target', generation: original.generation, configuration: updated },
    { name: 'target', generation: original.generation, configuration: configuration('alpine:3.20') },
  ]);
});

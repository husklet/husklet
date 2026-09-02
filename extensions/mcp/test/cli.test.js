import assert from 'node:assert/strict';
import { spawn, spawnSync } from 'node:child_process';
import fs from 'node:fs';
import net from 'node:net';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { Client } from '@modelcontextprotocol/sdk/client/index.js';
import { StdioClientTransport } from '@modelcontextprotocol/sdk/client/stdio.js';
import { CONTROL, KIND, Reader, encode } from '../../react/src/wire.js';
import { assertWorkspace, parseCli } from '../src/cli-options.js';

const cli = path.resolve(import.meta.dirname, '../src/cli.js');
const delay = (milliseconds) => new Promise((resolve) => setTimeout(resolve, milliseconds));
const until = async (condition, milliseconds = 2_000) => {
  const deadline = Date.now() + milliseconds;
  while (!condition()) {
    if (Date.now() >= deadline) throw new Error(`condition did not settle within ${milliseconds}ms`);
    await delay(10);
  }
};

async function fakeHost(context, { greet = true } = {}) {
  const scratch = fs.mkdtempSync(path.join(os.tmpdir(), 'husklet-mcp-cli-'));
  const socketPath = path.join(scratch, 'host.sock');
  const calls = [];
  const connections = new Set();
  const host = net.createServer((socket) => {
    connections.add(socket);
    socket.once('close', () => connections.delete(socket));
    if (!greet) return;
    const reader = new Reader();
    socket.write(encode({ channel: CONTROL, kind: KIND.request, payload: {
      protocol: 1, extension: 'observer', peer: 'observer', granted: ['workspace-read', 'container-attach'],
    } }));
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.channel === CONTROL || frame.kind !== KIND.request) continue;
        calls.push(frame.payload);
        const payload = frame.payload.call === 'workspace_info'
          ? { reply: 'workspace', with: { name: 'dev' } }
          : frame.payload.call === 'container_attach_terminal'
            ? { reply: 'identity', with: 'p-attached' }
            : null;
        if (!payload) throw new Error(`unexpected host call ${frame.payload.call}`);
        socket.write(encode({ channel: frame.channel, kind: KIND.response, payload }));
      }
    });
  });
  await new Promise((resolve, reject) => host.listen(socketPath, resolve).once('error', reject));
  context.after(async () => {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => host.close(resolve));
    fs.rmSync(scratch, { recursive: true, force: true });
  });
  return { socketPath, calls, connections };
}

test('CLI arguments require one bounded socket and workspace without accepting extras', () => {
  assert.deepEqual(parseCli(['--socket', '/tmp/h.sock', '--workspace', 'dev']), {
    help: false, socket: '/tmp/h.sock', workspace: 'dev',
  });
  assert.deepEqual(parseCli(['--help']), { help: true });
  for (const argv of [
    [], ['--socket', '/tmp/h.sock'], ['--workspace', 'dev'],
    ['--socket', '/tmp/a', '--socket', '/tmp/b', '--workspace', 'dev'],
    ['--socket', '/tmp/a', '--workspace', 'dev', '--extra', 'x'],
    ['--socket', '/tmp/a\0b', '--workspace', 'dev'],
  ]) assert.throws(() => parseCli(argv));

  const failed = spawnSync(process.execPath, [cli, '--socket', '/tmp/a', '--workspace', 'dev', '--extra', 'x'], { encoding: 'utf8' });
  assert.equal(failed.status, 1);
  assert.equal(failed.stdout, '');
  assert.match(failed.stderr, /unknown argument.*--extra/);
  assert.match(failed.stderr, /--help/);

  assert.doesNotThrow(() => assertWorkspace({ name: 'dev' }, 'dev'));
  assert.throws(() => assertWorkspace({ name: 'production' }, 'dev'), /hosts workspace "production", expected "dev"/);
  assert.throws(() => assertWorkspace({}, 'dev'), /hosts workspace null, expected "dev"/);
});

test('spawned packaged CLI initializes stdio MCP and lists tools through a real Unix session', async (context) => {
  const { socketPath, calls, connections } = await fakeHost(context);

  const transport = new StdioClientTransport({
    command: process.execPath,
    args: [cli, '--socket', socketPath, '--workspace', 'dev'],
    cwd: path.resolve(import.meta.dirname, '..'),
    stderr: 'pipe',
  });
  let diagnostics = '';
  transport.stderr.on('data', (chunk) => { diagnostics += chunk; });
  const client = new Client({ name: 'spawned-cli-test', version: '1' });
  await client.connect(transport);
  const listed = await client.listTools();
  assert(listed.tools.some(({ name }) => name === 'husklet_workspace_info'));
  assert(listed.tools.some(({ name }) => name === 'husklet_container_attach_terminal'));
  const answer = await client.callTool({ name: 'husklet_workspace_info', arguments: {} });
  assert.equal(JSON.parse(answer.content[0].text).name, 'dev');
  const id = 'a'.repeat(64);
  const attached = await client.callTool({ name: 'husklet_container_attach_terminal', arguments: { id, command: ['sh', '-i'] } });
  assert.equal(JSON.parse(attached.content[0].text), 'p-attached');
  assert.deepEqual(calls, [
    { call: 'workspace_info' }, { call: 'workspace_info' },
    { call: 'container_attach_terminal', with: { id, command: ['sh', '-i'] } },
  ]);
  assert.equal(diagnostics, '');
  await client.close();
  await until(() => connections.size === 0);
  assert.equal(diagnostics, '');
});

test('startup handshake timeout is bounded, actionable, and leaves no socket', async (context) => {
  const { socketPath, connections } = await fakeHost(context, { greet: false });
  const child = spawn(process.execPath, [cli, '--socket', socketPath, '--workspace', 'dev'], { stdio: ['pipe', 'pipe', 'pipe'] });
  let stdout = ''; let stderr = '';
  child.stdout.on('data', (chunk) => { stdout += chunk; });
  child.stderr.on('data', (chunk) => { stderr += chunk; });
  const code = await new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error('CLI did not honor its startup timeout')), 7_000);
    child.once('exit', (value) => { clearTimeout(timer); resolve(value); });
  });
  assert.equal(code, 1);
  assert.equal(stdout, '');
  assert.match(stderr, /startup failed: extension host handshake timed out after 5000ms/);
  await until(() => connections.size === 0);
});

test('host EOF terminates instead of reconnecting or leaving the MCP child alive', async (context) => {
  const { socketPath, connections } = await fakeHost(context);
  const transport = new StdioClientTransport({ command: process.execPath, args: [cli, '--socket', socketPath, '--workspace', 'dev'], stderr: 'pipe' });
  let diagnostics = '';
  transport.stderr.on('data', (chunk) => { diagnostics += chunk; });
  const client = new Client({ name: 'host-eof-test', version: '1' });
  await client.connect(transport);
  const closed = new Promise((resolve) => { transport.onclose = resolve; });
  for (const connection of connections) connection.destroy();
  await Promise.race([closed, delay(2_000).then(() => { throw new Error('MCP child survived host EOF'); })]);
  assert.match(diagnostics, /host authority connection ended: extension host connection closed/);
  assert.equal(connections.size, 0);
});

for (const signal of ['SIGINT', 'SIGTERM']) test(`${signal} closes MCP and host sockets without diagnostics`, async (context) => {
  const { socketPath, connections } = await fakeHost(context);
  const transport = new StdioClientTransport({ command: process.execPath, args: [cli, '--socket', socketPath, '--workspace', 'dev'], stderr: 'pipe' });
  let diagnostics = '';
  transport.stderr.on('data', (chunk) => { diagnostics += chunk; });
  const client = new Client({ name: `signal-${signal}`, version: '1' });
  await client.connect(transport);
  const closed = new Promise((resolve) => { transport.onclose = resolve; });
  process.kill(transport.pid, signal);
  await Promise.race([closed, delay(2_000).then(() => { throw new Error(`MCP child survived ${signal}`); })]);
  await until(() => connections.size === 0);
  assert.equal(diagnostics, '');
});

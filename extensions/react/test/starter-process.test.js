import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import fs from 'node:fs';
import net from 'node:net';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';
import { KIND, Reader, encode } from '../src/wire.js';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const starter = path.join(root, 'examples/starter');
const delay = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

async function until(predicate, message) {
  for (let attempt = 0; attempt < 400; attempt += 1) {
    if (predicate()) return;
    await delay(5);
  }
  throw new Error(message);
}

function launch(socket) {
  const env = { ...process.env };
  if (socket === undefined) delete env.HUSKLET_EXTENSION_SOCKET;
  else env.HUSKLET_EXTENSION_SOCKET = socket;
  const child = spawn(process.execPath, ['main.js'], { cwd: starter, env, stdio: ['ignore', 'pipe', 'pipe'] });
  let stdout = ''; let stderr = '';
  child.stdout.on('data', (chunk) => { stdout += chunk; });
  child.stderr.on('data', (chunk) => { stderr += chunk; });
  return { child, output: () => ({ stdout, stderr }) };
}

async function exited(child) {
  return Promise.race([
    new Promise((resolve) => child.once('exit', (code, signal) => resolve({ code, signal }))),
    delay(2_000).then(() => { throw new Error('starter child did not exit'); }),
  ]);
}

async function host(context, protocol = 1) {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'husklet-starter-process-'));
  context.after(() => fs.rmSync(directory, { recursive: true, force: true }));
  const socket = path.join(directory, 'host.sock');
  const connections = new Set();
  let rendered = false;
  const server = net.createServer((stream) => {
    connections.add(stream);
    stream.on('close', () => connections.delete(stream));
    const reader = new Reader();
    stream.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.channel === 0 || frame.kind !== KIND.request) continue;
        if (frame.payload.call === 'interface_render_at') rendered = true;
        stream.write(encode({ channel: frame.channel, kind: KIND.response, payload: frame.payload.call === 'interface_open_tab' ? { reply: 'identity', with: 'starter-pane' } : { reply: 'done' } }));
      }
    });
    stream.write(encode({ channel: 0, kind: KIND.open, payload: { protocol, extension: 'react-starter', granted: ['interface'] } }));
  });
  await new Promise((resolve, reject) => { server.once('error', reject); server.listen(socket, resolve); });
  context.after(() => new Promise((resolve) => server.close(resolve)));
  return { socket, connections, rendered: () => rendered };
}

test('missing socket is actionable and exits nonzero without a stack dump', async () => {
  const run = launch();
  assert.deepEqual(await exited(run.child), { code: 1, signal: null });
  assert.equal(run.output().stdout, '');
  assert.equal(run.output().stderr, 'react-starter: startup failed: HUSKLET_EXTENSION_SOCKET is not set; an extension runs inside a workspace\n');
});

test('unexpected host EOF after readiness is a visible nonzero failure without reconnect', async (context) => {
  const fake = await host(context);
  const run = launch(fake.socket);
  await until(fake.rendered, 'starter never rendered');
  assert.equal(fake.connections.size, 1);
  for (const connection of fake.connections) connection.destroy();
  assert.deepEqual(await exited(run.child), { code: 1, signal: null });
  assert.match(run.output().stderr, /^react-starter: host connection ended: extension host connection closed\n$/);
  assert.equal(fake.connections.size, 0);
});

test('protocol refusal is reported as startup failure', async (context) => {
  const fake = await host(context, 2);
  const run = launch(fake.socket);
  assert.deepEqual(await exited(run.child), { code: 1, signal: null });
  assert.match(run.output().stderr, /startup failed: host speaks protocol 2, this extension speaks 1/);
});

for (const signal of ['SIGINT', 'SIGTERM']) test(`${signal} closes the ready socket cleanly`, async (context) => {
  const fake = await host(context);
  const run = launch(fake.socket);
  await until(fake.rendered, 'starter never rendered');
  run.child.kill(signal);
  assert.deepEqual(await exited(run.child), { code: 0, signal: null });
  await until(() => fake.connections.size === 0, 'starter left its socket open');
  assert.deepEqual(run.output(), { stdout: '', stderr: '' });
});

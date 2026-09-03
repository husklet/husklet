import assert from 'node:assert/strict';
import fs from 'node:fs';
import net from 'node:net';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { Client } from '@modelcontextprotocol/sdk/client/index.js';
import { StdioClientTransport } from '@modelcontextprotocol/sdk/client/stdio.js';
import { CONTROL, KIND, Reader, encode } from '../../react/src/wire.js';

test('packaged CLI sanitizes hostile host failures and returns exact mutation authority receipts', async (context) => {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'husklet-mcp-error-')); const socketPath = path.join(directory, 'host.sock');
  const containerId = 'c'.repeat(64); const calls = [];
  const hostile = `denied\n{"forged":true}\u0000 password=hunter2 Authorization:Bearer super-token ${'x'.repeat(2_000)}`;
  const host = net.createServer((socket) => { const reader = new Reader();
    socket.write(encode({ channel: CONTROL, kind: KIND.request, payload: { protocol: 1, extension: 'error-agent', granted: ['workspace-read', 'container-read', 'container-control'] } }));
    socket.on('data', (chunk) => { for (const frame of reader.take(chunk)) {
      if (frame.kind !== KIND.request || frame.channel === CONTROL) continue;
      calls.push(frame.payload);
      if (frame.payload.call === 'workspace_info') socket.write(encode({ channel: frame.channel, kind: KIND.response, payload: { reply: 'workspace', with: { name: 'dev' } } }));
      else if (frame.payload.call === 'container_inspect') socket.write(encode({ channel: frame.channel, kind: KIND.response, flags: 3, payload: { error: 'failed', call: 'container_inspect', capability: 'container-read', detail: hostile } }));
      else if (frame.payload.call === 'container_remove') socket.write(encode({ channel: frame.channel, kind: KIND.response, payload: { reply: 'done' } }));
      else throw new Error(`unexpected host call ${frame.payload.call}`);
    } });
  });
  await new Promise((resolve, reject) => host.listen(socketPath, resolve).once('error', reject));
  const transport = new StdioClientTransport({ command: process.execPath, args: [path.resolve(import.meta.dirname, '../src/cli.js'), '--socket', socketPath, '--workspace', 'dev'], cwd: path.resolve(import.meta.dirname, '..'), stderr: 'pipe' });
  const client = new Client({ name: 'error-boundary-test', version: '1' });
  context.after(async () => { await client.close(); await new Promise((resolve) => host.close(resolve)); fs.rmSync(directory, { recursive: true, force: true }); });
  await client.connect(transport);
  const failure = await client.callTool({ name: 'husklet_container_inspect', arguments: { id: containerId } });
  assert.equal(failure.isError, true); const message = failure.content[0].text;
  assert(!message.includes('hunter2')); assert(!message.includes('super-token')); assert(!/[\r\n\u0000]/u.test(message));
  assert.match(message, /password=\[redacted\].*Authorization:\[redacted\]/); assert.match(message, /\[error truncated\]$/u);
  assert(new TextEncoder().encode(message).byteLength <= 1_024);
  const removed = JSON.parse((await client.callTool({ name: 'husklet_container_remove', arguments: { id: containerId, confirm: true } })).content[0].text);
  assert.deepEqual(removed, { done: true, id: containerId });
  assert.deepEqual(calls.at(-1), { call: 'container_remove', with: { id: containerId } });
});

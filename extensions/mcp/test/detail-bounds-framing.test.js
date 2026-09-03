import assert from 'node:assert/strict';
import fs from 'node:fs';
import net from 'node:net';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { Client } from '@modelcontextprotocol/sdk/client/index.js';
import { StdioClientTransport } from '@modelcontextprotocol/sdk/client/stdio.js';
import { CONTROL, KIND, Reader, encode } from '../../react/src/wire.js';

const configuration = (name, mounts = []) => ({ name, generation: 'a'.repeat(32), image: 'alpine:3.20', architecture: 'x86_64', storage: null, shell: null, cpus: null, memory_mb: null, environment: [['API_TOKEN', 'do-not-expose'], ['MODE', 'development']], mounts, docker_socket: false, scrollback: null, vpn: null, execution_lifetime: 'persisted', terminal: { font_family: null, font_size: null, foreground: null, background: null, cursor_shape: null, cursor_blink: null } });

test('packaged CLI redacts secret tuples and fails closed instead of clipping detail objects', async (context) => {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'husklet-mcp-detail-')); const socketPath = path.join(directory, 'host.sock');
  const containerId = 'c'.repeat(64); const oversized = Array.from({ length: 201 }, (_, index) => ({ host: `/host/${index}`, container: `/guest/${index}`, read_only: true }));
  const host = net.createServer((socket) => { const reader = new Reader();
    socket.write(encode({ channel: CONTROL, kind: KIND.request, payload: { protocol: 1, extension: 'detail-agent', granted: ['workspace-read', 'container-read'] } }));
    socket.on('data', (chunk) => { for (const frame of reader.take(chunk)) {
      if (frame.kind !== KIND.request || frame.channel === CONTROL) continue;
      let payload;
      if (frame.payload.call === 'workspace_info') payload = { reply: 'workspace', with: { name: 'dev' } };
      else if (frame.payload.call === 'workspace_inspect') payload = { reply: 'workspace_configuration', with: configuration(frame.payload.with.name, frame.payload.with.name === 'oversized' ? oversized : []) };
      else if (frame.payload.call === 'container_inspect') payload = { reply: 'container', with: { id: containerId, name: 'build', image: 'alpine:3.20', state: 'running', created: 7 } };
      else throw new Error(`unexpected host call ${frame.payload.call}`);
      socket.write(encode({ channel: frame.channel, kind: KIND.response, payload }));
    } });
  });
  await new Promise((resolve, reject) => host.listen(socketPath, resolve).once('error', reject));
  const transport = new StdioClientTransport({ command: process.execPath, args: [path.resolve(import.meta.dirname, '../src/cli.js'), '--socket', socketPath, '--workspace', 'dev'], cwd: path.resolve(import.meta.dirname, '..'), stderr: 'pipe' });
  const client = new Client({ name: 'detail-bounds-test', version: '1' });
  context.after(async () => { await client.close(); await new Promise((resolve) => host.close(resolve)); fs.rmSync(directory, { recursive: true, force: true }); });
  await client.connect(transport);
  const safe = JSON.parse((await client.callTool({ name: 'husklet_workspace_inspect', arguments: { name: 'safe' } })).content[0].text);
  assert.deepEqual(safe.environment, [['API_TOKEN', '[redacted]'], ['MODE', 'development']]); assert.equal(safe.generation, 'a'.repeat(32));
  const refused = await client.callTool({ name: 'husklet_workspace_inspect', arguments: { name: 'oversized' } });
  assert.equal(refused.isError, true); assert.match(refused.content[0].text, /nested array exceeds/);
  const container = JSON.parse((await client.callTool({ name: 'husklet_container_inspect', arguments: { id: containerId } })).content[0].text);
  assert.equal(container.id, containerId); assert.equal(container.state, 'running');
});

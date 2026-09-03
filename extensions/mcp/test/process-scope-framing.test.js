import assert from 'node:assert/strict';
import fs from 'node:fs';
import net from 'node:net';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { Client } from '@modelcontextprotocol/sdk/client/index.js';
import { StdioClientTransport } from '@modelcontextprotocol/sdk/client/stdio.js';
import { CONTROL, KIND, Reader, encode } from '../../react/src/wire.js';

test('spawned packaged CLI preserves initial and namespace process scopes over Unix framing', async (context) => {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'husklet-mcp-process-scope-'));
  const socketPath = path.join(directory, 'host.sock'); const container = 'c'.repeat(64); let reads = 0;
  const host = net.createServer((socket) => { const reader = new Reader();
    socket.write(encode({ channel: CONTROL, kind: KIND.request, payload: { protocol: 1, extension: 'process-agent', granted: ['workspace-read', 'container-read'] } }));
    socket.on('data', (chunk) => { for (const frame of reader.take(chunk)) {
      if (frame.kind !== KIND.request || frame.channel === CONTROL) continue;
      let payload;
      if (frame.payload.call === 'workspace_info') payload = { reply: 'workspace', with: { name: 'dev' } };
      else if (frame.payload.call === 'container_processes') { reads += 1; const scope = reads === 1 ? 'initial' : 'namespace'; payload = { reply: 'processes', with: { container_id: container, titles: ['PID', 'PPID', 'COMMAND'], processes: scope === 'initial' ? [['1', '0', 'init']] : [['1', '0', 'init'], ['7', '1', 'worker']], observed_at_ms: 1_700_000_000_000 + reads, scope, pid_identity: 'snapshot', truncated: false } }; }
      else throw new Error(`unexpected host call ${frame.payload.call}`);
      socket.write(encode({ channel: frame.channel, kind: KIND.response, payload }));
    } });
  });
  await new Promise((resolve, reject) => host.listen(socketPath, resolve).once('error', reject));
  const transport = new StdioClientTransport({ command: process.execPath, args: [path.resolve(import.meta.dirname, '../src/cli.js'), '--socket', socketPath, '--workspace', 'dev'], cwd: path.resolve(import.meta.dirname, '..'), stderr: 'pipe' });
  const client = new Client({ name: 'process-scope-test', version: '1' });
  context.after(async () => { await client.close(); await new Promise((resolve) => host.close(resolve)); fs.rmSync(directory, { recursive: true, force: true }); });
  await client.connect(transport);
  const definition = (await client.listTools()).tools.find(({ name }) => name === 'husklet_container_processes');
  assert.match(definition.description, /scope says initial or full namespace/);
  const read = async () => JSON.parse((await client.callTool({ name: 'husklet_container_processes', arguments: { id: container } })).content[0].text);
  const initial = await read(); const namespace = await read();
  assert.deepEqual({ scope: initial.scope, rows: initial.processes.length, pid: initial.pid_identity }, { scope: 'initial', rows: 1, pid: 'snapshot' });
  assert.deepEqual({ scope: namespace.scope, rows: namespace.processes.length, pid: namespace.pid_identity }, { scope: 'namespace', rows: 2, pid: 'snapshot' });
  assert.equal(namespace.container_id, container); assert.equal(namespace.truncated, false);
});

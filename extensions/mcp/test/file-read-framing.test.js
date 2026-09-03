import assert from 'node:assert/strict';
import fs from 'node:fs';
import net from 'node:net';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { Client } from '@modelcontextprotocol/sdk/client/index.js';
import { StdioClientTransport } from '@modelcontextprotocol/sdk/client/stdio.js';
import { CONTROL, KIND, Reader, encode } from '../../react/src/wire.js';

test('packaged CLI returns exact whole files or explicit independently observed ranges', async (context) => {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'husklet-mcp-file-'));
  const socketPath = path.join(directory, 'host.sock'); const file = Array.from({ length: 12_050 }, (_, index) => index % 256); const reads = [];
  const host = net.createServer((socket) => { const reader = new Reader();
    socket.write(encode({ channel: CONTROL, kind: KIND.request, payload: { protocol: 1, extension: 'file-agent', granted: ['workspace-read', 'filesystem-read'] } }));
    socket.on('data', (chunk) => { for (const frame of reader.take(chunk)) {
      if (frame.kind !== KIND.request || frame.channel === CONTROL) continue;
      let payload;
      if (frame.payload.call === 'workspace_info') payload = { reply: 'workspace', with: { name: 'dev' } };
      else if (frame.payload.call === 'filesystem_read') { reads.push(frame.payload.with.path); payload = { reply: 'contents', with: frame.payload.with.path === 'small.bin' ? file.slice(0, 257) : file }; }
      else throw new Error(`unexpected host call ${frame.payload.call}`);
      socket.write(encode({ channel: frame.channel, kind: KIND.response, payload }));
    } });
  });
  await new Promise((resolve, reject) => host.listen(socketPath, resolve).once('error', reject));
  const transport = new StdioClientTransport({ command: process.execPath, args: [path.resolve(import.meta.dirname, '../src/cli.js'), '--socket', socketPath, '--workspace', 'dev'], cwd: path.resolve(import.meta.dirname, '..'), stderr: 'pipe' });
  const client = new Client({ name: 'file-read-test', version: '1' });
  context.after(async () => { await client.close(); await new Promise((resolve) => host.close(resolve)); fs.rmSync(directory, { recursive: true, force: true }); });
  await client.connect(transport);
  const definitions = Object.fromEntries((await client.listTools()).tools.map((tool) => [tool.name, tool]));
  assert.match(definitions.husklet_file_read.description, /complete contents.*12000-byte MCP whole-read limit.*fail closed/);
  assert.match(definitions.husklet_file_read_range.description, /independent observation.*no immutable snapshot identity/);
  const small = await client.callTool({ name: 'husklet_file_read', arguments: { path: 'small.bin' } });
  assert.deepEqual(JSON.parse(small.content[0].text), file.slice(0, 257));
  const refused = await client.callTool({ name: 'husklet_file_read', arguments: { path: 'large.bin' } });
  assert.equal(refused.isError, true); assert.match(refused.content[0].text, /use husklet_file_read_range/);
  const first = JSON.parse((await client.callTool({ name: 'husklet_file_read_range', arguments: { path: 'large.bin', offset: 0, limit: 12_000 } })).content[0].text);
  assert.deepEqual({ path: first.path, offset: first.offset, total: first.total, length: first.contents.length, eof: first.eof, truncated: first.truncated }, { path: 'large.bin', offset: 0, total: 12_050, length: 12_000, eof: false, truncated: true });
  const last = JSON.parse((await client.callTool({ name: 'husklet_file_read_range', arguments: { path: 'large.bin', offset: 12_000, limit: 100 } })).content[0].text);
  assert.deepEqual({ offset: last.offset, contents: last.contents, eof: last.eof, truncated: last.truncated }, { offset: 12_000, contents: file.slice(12_000), eof: true, truncated: false });
  assert.deepEqual(reads, ['small.bin', 'large.bin', 'large.bin', 'large.bin']);
});

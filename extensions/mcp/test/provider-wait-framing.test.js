import assert from 'node:assert/strict';
import fs from 'node:fs';
import net from 'node:net';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { Client } from '@modelcontextprotocol/sdk/client/index.js';
import { StdioClientTransport } from '@modelcontextprotocol/sdk/client/stdio.js';
import { CONTROL, KIND, Reader, encode } from '../../react/src/wire.js';

test('packaged CLI waits for a newly fenced mounted provider over real Unix framing', async (context) => {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'husklet-provider-wait-')); const socketPath = path.join(directory, 'host.sock');
  let reads = 0; let subscriptions = 0; let credits = 0; let present = true;
  const pane = (revision) => ({ slot: 'manager-pane', generation: 4, revision, kind: 'surface', provider: { extension: 'workspace-manager', provider: 'main' }, tab: 'tools', title: 'Manager', focused: false });
  const host = net.createServer((socket) => { const reader = new Reader();
    socket.write(encode({ channel: CONTROL, kind: KIND.request, payload: { protocol: 1, extension: 'provider-agent', granted: ['workspace-read', 'extension-read', 'pane-observe'] } }));
    socket.on('data', (chunk) => { for (const frame of reader.take(chunk)) {
      if (frame.kind === KIND.credit) { credits += 1; continue; }
      if (frame.kind !== KIND.request || frame.channel === CONTROL) continue;
      let payload;
      if (frame.payload.call === 'workspace_info') payload = { reply: 'workspace', with: { name: 'dev' } };
      else if (frame.payload.call === 'event_subscribe') { subscriptions += 1; payload = { reply: 'done' }; }
      else if (frame.payload.call === 'event_unsubscribe') { subscriptions -= 1; present = false; payload = { reply: 'done' }; }
      else if (frame.payload.call === 'pane_list') {
        reads += 1; payload = { reply: 'panes', with: { panes: present ? [pane(reads === 1 ? 8 : 9)] : [], truncated: false } };
        if (reads === 1) setImmediate(() => socket.write(encode({ channel: 11, kind: KIND.event, payload: { snapshot: 'pane_changes', of: { slot: 'manager-pane', generation: 4, revision: 9, kind: 'surface', coalesced: 0 } } })));
      } else throw new Error(`unexpected host call ${frame.payload.call}`);
      socket.write(encode({ channel: frame.channel, kind: KIND.response, payload }));
    } });
  });
  await new Promise((resolve, reject) => host.listen(socketPath, resolve).once('error', reject));
  const transport = new StdioClientTransport({ command: process.execPath, args: [path.resolve(import.meta.dirname, '../src/cli.js'), '--socket', socketPath, '--workspace', 'dev'], cwd: path.resolve(import.meta.dirname, '..'), stderr: 'pipe' });
  const client = new Client({ name: 'provider-wait-test', version: '1' });
  context.after(async () => { await client.close(); await new Promise((resolve) => host.close(resolve)); fs.rmSync(directory, { recursive: true, force: true }); });
  await client.connect(transport);
  const result = await client.callTool({ name: 'husklet_extension_provider_wait', arguments: { extension: 'workspace-manager', provider: 'main', state: 'mounted', after: { slot: 'manager-pane', generation: 4, revision: 8 }, timeout_ms: 1_000 } });
  assert.deepEqual(JSON.parse(result.content[0].text), { changed: true, state: 'mounted', pane: pane(9), truncated: false });
  const removed = await client.callTool({ name: 'husklet_extension_provider_wait', arguments: { extension: 'workspace-manager', provider: 'main', state: 'unmounted', after: { slot: 'manager-pane', generation: 4, revision: 9 }, timeout_ms: 1_000 } });
  assert.deepEqual(JSON.parse(removed.content[0].text), { changed: true, state: 'unmounted', pane: null, truncated: false });
  assert.equal(reads, 3); assert.equal(subscriptions, 0); assert.equal(credits, 1);
});

import assert from 'node:assert/strict';
import fs from 'node:fs';
import net from 'node:net';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { Client } from '@modelcontextprotocol/sdk/client/index.js';
import { StdioClientTransport } from '@modelcontextprotocol/sdk/client/stdio.js';
import { CONTROL, KIND, Reader, encode } from '../../react/src/wire.js';

test('packaged pane mutation subscribes before authority and disposes over real Unix framing', async (context) => {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'husklet-pane-mutate-'));
  const socketPath = path.join(directory, 'host.sock'); const calls = []; let subscriptions = 0; let credits = 0;
  const host = net.createServer((socket) => { const reader = new Reader();
    socket.write(encode({ channel: CONTROL, kind: KIND.request, payload: { protocol: 1, extension: 'pane-agent', granted: ['workspace-read', 'terminal-control', 'pane-observe'] } }));
    socket.on('data', (chunk) => { for (const frame of reader.take(chunk)) {
      if (frame.kind === KIND.credit) { credits += 1; continue; }
      if (frame.kind !== KIND.request || frame.channel === CONTROL) continue;
      const call = frame.payload.call; calls.push(call); let payload; let flags = 0;
      if (call === 'workspace_info') payload = { reply: 'workspace', with: { name: 'dev' } };
      else if (call === 'event_subscribe') { subscriptions += 1; payload = { reply: 'done' }; }
      else if (call === 'event_unsubscribe') { subscriptions -= 1; payload = { reply: 'done' }; }
      else if (call === 'terminal_retitle_pane_observed') {
        assert.equal(subscriptions, 1, 'authority ran before subscribe acknowledgement');
        payload = { reply: 'done' };
        setImmediate(() => socket.write(encode({ channel: 11, kind: KIND.event, payload: { snapshot: 'pane_changes', of: { slot: 'pane-live', generation: 4, revision: 10, kind: 'terminal', coalesced: 0 } } })));
      } else if (call === 'terminal_focus_pane_observed') { flags = 3; payload = { error: 'conflict', detail: 'stale pane' }; }
      else throw new Error(`unexpected host call ${call}`);
      socket.write(encode({ channel: frame.channel, kind: KIND.response, flags, payload }));
    } });
  });
  await new Promise((resolve, reject) => host.listen(socketPath, resolve).once('error', reject));
  const transport = new StdioClientTransport({ command: process.execPath, args: [path.resolve(import.meta.dirname, '../src/cli.js'), '--socket', socketPath, '--workspace', 'dev'], cwd: path.resolve(import.meta.dirname, '..'), stderr: 'pipe' });
  const client = new Client({ name: 'pane-mutate-test', version: '1' });
  context.after(async () => { await client.close(); await new Promise((resolve) => host.close(resolve)); fs.rmSync(directory, { recursive: true, force: true }); });
  await client.connect(transport);

  const changed = await client.callTool({ name: 'husklet_terminal_mutate_wait', arguments: { action: 'retitle', slot: 'pane-live', generation: 4, revision: 9, title: 'Build', timeout_ms: 1000 } });
  assert.equal(JSON.parse(changed.content[0].text).observation.change.revision, 10);
  const failed = await client.callTool({ name: 'husklet_terminal_mutate_wait', arguments: { action: 'focus', slot: 'pane-live', generation: 4, revision: 10, timeout_ms: 1000 } });
  assert.equal(failed.isError, true); assert.match(failed.content[0].text, /stale pane/);
  assert.equal(subscriptions, 0); assert.equal(credits, 1);
  assert.deepEqual(calls.slice(1), ['event_subscribe', 'terminal_retitle_pane_observed', 'event_unsubscribe', 'event_subscribe', 'terminal_focus_pane_observed', 'event_unsubscribe']);
});

import assert from 'node:assert/strict';
import fs from 'node:fs';
import net from 'node:net';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { Client } from '@modelcontextprotocol/sdk/client/index.js';
import { StdioClientTransport } from '@modelcontextprotocol/sdk/client/stdio.js';
import { CONTROL, KIND, Reader, encode } from '../../react/src/wire.js';

test('packaged CLI lists and waits for enabled digest-bound provider declarations', async (context) => {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'husklet-provider-catalogue-')); const socketPath = path.join(directory, 'host.sock');
  const digest = `sha256:${'d'.repeat(64)}`; let credits = 0; let subscribed = 0;
  const providers = Array.from({ length: 201 }, (_, index) => ({ id: `provider-${index}`, title: `Provider ${index}`, icon: null }));
  const manager = (status = 'duty') => ({ name: 'manager', image_digest: digest, status, version: '2.0.0', enabled: true, pane_providers: providers });
  const disabled = { name: 'disabled', image_digest: `sha256:${'e'.repeat(64)}`, status: 'standby', version: '1.0.0', enabled: false, pane_providers: [{ id: 'hidden', title: 'Hidden', icon: null }] };
  const host = net.createServer((socket) => { const reader = new Reader();
    socket.write(encode({ channel: CONTROL, kind: KIND.request, payload: { protocol: 1, extension: 'catalogue-agent', granted: ['workspace-read', 'extension-read'] } }));
    socket.on('data', (chunk) => { for (const frame of reader.take(chunk)) {
      if (frame.kind === KIND.credit) { credits += 1; continue; }
      if (frame.kind !== KIND.request || frame.channel === CONTROL) continue;
      let payload;
      if (frame.payload.call === 'workspace_info') payload = { reply: 'workspace', with: { name: 'dev' } };
      else if (frame.payload.call === 'extension_list') payload = { reply: 'extensions', with: [disabled, manager()] };
      else if (frame.payload.call === 'event_subscribe') {
        subscribed += 1; payload = { reply: 'done' };
        setImmediate(() => {
          socket.write(encode({ channel: 12, kind: KIND.event, payload: { snapshot: 'extensions', of: [disabled, manager()] } }));
          socket.write(encode({ channel: 13, kind: KIND.event, payload: { snapshot: 'extensions', of: [disabled, manager('fault:1')] } }));
        });
      } else if (frame.payload.call === 'event_unsubscribe') { subscribed -= 1; payload = { reply: 'done' }; }
      else throw new Error(`unexpected host call ${frame.payload.call}`);
      socket.write(encode({ channel: frame.channel, kind: KIND.response, payload }));
    } });
  });
  await new Promise((resolve, reject) => host.listen(socketPath, resolve).once('error', reject));
  const transport = new StdioClientTransport({ command: process.execPath, args: [path.resolve(import.meta.dirname, '../src/cli.js'), '--socket', socketPath, '--workspace', 'dev'], cwd: path.resolve(import.meta.dirname, '..'), stderr: 'pipe' });
  const client = new Client({ name: 'provider-catalogue-test', version: '1' });
  context.after(async () => { await client.close(); await new Promise((resolve) => host.close(resolve)); fs.rmSync(directory, { recursive: true, force: true }); });
  await client.connect(transport);
  const listed = JSON.parse((await client.callTool({ name: 'husklet_extension_provider_list', arguments: {} })).content[0].text);
  assert.equal(listed.providers.length, 200); assert.equal(listed.truncated, true);
  assert.deepEqual(listed.providers[0], { extension: 'manager', image_digest: digest, version: '2.0.0', status: 'duty', id: 'provider-0', title: 'Provider 0', icon: null });
  assert(!listed.providers.some(({ extension }) => extension === 'disabled'));
  const changed = JSON.parse((await client.callTool({ name: 'husklet_extension_provider_catalogue_wait', arguments: { after: { name: 'manager', image_digest: digest, status: 'duty' }, timeout_ms: 1_000 } })).content[0].text);
  assert.equal(changed.extension.status, 'fault:1'); assert.equal(changed.catalogue.providers[0].status, 'fault:1'); assert.equal(changed.catalogue.truncated, true);
  assert.equal(subscribed, 0); assert.equal(credits, 2);
});

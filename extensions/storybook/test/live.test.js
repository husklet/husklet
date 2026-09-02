import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import fs from 'node:fs';
import net from 'node:net';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';

import { PACKAGE } from './host.js';
import { tags } from '../src/catalogue.js';

const { KIND, Reader, encode } = await import(new URL('src/wire.js', `file://${PACKAGE}`));

async function until(condition, message) {
  for (let attempt = 0; attempt < 400; attempt += 1) {
    const value = condition();
    if (value) return value;
    await new Promise((resolve) => setTimeout(resolve, 5));
  }
  throw new Error(message);
}

test('the shipped entrypoint connects and renders the complete playground over a real socket', async (context) => {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'husklet-storybook-live-'));
  const socket = path.join(directory, 'extension.sock');
  const calls = [];
  let accepted;
  const server = net.createServer((stream) => {
    accepted = stream;
    const reader = new Reader();
    stream.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.channel === 0) continue;
        if (frame.kind !== KIND.request) continue;
        calls.push(frame.payload);
        const payload = frame.payload.call === 'interface_open_tab'
          ? { reply: 'identity', with: 'storybook-main' }
          : { reply: 'done' };
        stream.write(encode({
          channel: frame.channel,
          kind: KIND.response,
          payload,
        }));
      }
    });
    stream.write(encode({
      channel: 0,
      kind: KIND.open,
      payload: { protocol: 1, extension: 'storybook', granted: ['interface'] },
    }));
  });
  await new Promise((resolve, reject) => {
    server.once('error', reject);
    server.listen(socket, resolve);
  });

  const child = spawn(process.execPath, ['src/main.js'], {
    cwd: path.resolve(import.meta.dirname, '..'),
    env: { ...process.env, HUSKLET_EXTENSION_SOCKET: socket },
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  let stderr = '';
  child.stderr.setEncoding('utf8');
  child.stderr.on('data', (chunk) => (stderr += chunk));

  context.after(async () => {
    accepted?.destroy();
    if (child.exitCode === null) child.kill('SIGTERM');
    await new Promise((resolve) => child.once('exit', resolve));
    await new Promise((resolve) => server.close(resolve));
    fs.rmSync(directory, { recursive: true, force: true });
  });

  const rendered = await until(
    () => calls.find((call) => call.call === 'interface_render_at'),
    `storybook never rendered; stderr=${stderr}`,
  );
  const length = await until(
    () => calls.find((call) => call.call === 'source_resize_at'),
    `storybook never published its large source; stderr=${stderr}`,
  );
  assert.deepEqual(calls[0], { call: 'interface_open_tab', with: { title: 'Storybook' } });
  assert.equal(rendered.with.slot, 'storybook-main');
  assert.equal(rendered.with.frame.sequence, 1);
  assert.equal(length.with.slot, 'storybook-main');
  assert.deepEqual(length.with.mutation.Length, { source: 100, version: 1, rows: 100_000 });
  assert.ok(rendered.with.frame.patches.length > 250, 'the live frame does not contain the full component browser');
  assert.equal(
    rendered.with.frame.patches.filter((patch) => patch.Create?.tag === 'ListItemButton').length,
    tags.length + 5,
    'the live playground did not render the complete component catalogue and end-user flows',
  );
  assert.ok(
    rendered.with.frame.patches.some((patch) => patch.Create?.tag === 'Scroll'),
    'the live playground did not render its scrolling browser',
  );
  assert.equal(stderr, '');
});

import assert from 'node:assert/strict';
import net from 'node:net';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { createElement as h } from 'react';

import { connect, render } from '../src/index.js';
import { Button, Column } from '../src/components.js';
import { KIND, Reader, encode } from '../src/wire.js';
import { PROTOCOL } from '../src/session.js';

/** A host that greets, records calls, and can push an event back. */
async function host() {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'husklet-react-'));
  const socket = path.join(directory, 'extension.sock');
  const calls = [];
  let connected;
  const arrived = new Promise((resolve) => {
    connected = resolve;
  });
  let accepted = null;
  const server = net.createServer((stream) => {
    accepted = stream;
    const reader = new Reader();
    stream.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.kind === KIND.request && frame.channel !== 0) calls.push(frame.payload);
      }
    });
    stream.write(encode({ channel: 0, kind: KIND.open, payload: { protocol: PROTOCOL, extension: 'demo', granted: ['interface'] } }));
    connected(stream);
  });
  await new Promise((resolve) => server.listen(socket, resolve));
  return {
    socket,
    calls,
    stream: () => arrived,
    async push(payload) {
      (await arrived).write(encode({ channel: 2, kind: KIND.event, payload }));
    },
    close() {
      accepted?.destroy();
      server.close();
      fs.rmSync(directory, { recursive: true, force: true });
    },
  };
}

/** Waits for a condition the host reaches on its own schedule. */
async function until(condition) {
  for (let attempt = 0; attempt < 200; attempt += 1) {
    if (condition()) return;
    await new Promise((resolve) => setTimeout(resolve, 5));
  }
  throw new Error('the host never got there');
}

test('a tab is opened before the first frame is rendered', async () => {
  const stage = await host();
  const session = await connect({ path: stage.socket });
  render(h(Column, null, h(Button, { label: 'Go', onInvoke: () => {} })), session, { title: 'Demo' });
  await until(() => stage.calls.length >= 2);
  assert.deepEqual(stage.calls[0], { call: 'interface_open_tab', with: { title: 'Demo' } });
  assert.equal(stage.calls[1].call, 'interface_render');
  assert.equal(stage.calls[1].with.frame.sequence, 1);
  session.close();
  stage.close();
});

test('a handler runs when the host reports its event', async () => {
  const stage = await host();
  const session = await connect({ path: stage.socket });
  let invoked = 0;
  render(h(Column, null, h(Button, { label: 'Go', onInvoke: () => (invoked += 1) })), session, { title: 'Demo' });
  await until(() => stage.calls.length >= 2);

  await stage.push({ event: { Invoke: { node: 1, id: '1:Invoke' } } });
  await until(() => invoked === 1);

  // The other spelling the host may use, so a plain trigger field also lands.
  await stage.push({ event: 'Invoke', id: '1:Invoke', node: 1 });
  await until(() => invoked === 2);

  session.close();
  stage.close();
});

test('a re-render rebinds the callback without a patch', async () => {
  const stage = await host();
  const session = await connect({ path: stage.socket });
  let latest = 'first';
  const handle = render(h(Column, null, h(Button, { label: 'Go', onInvoke: () => (latest = 'first') })), session, {
    title: 'Demo',
  });
  await until(() => stage.calls.length >= 2);
  handle.update(h(Column, null, h(Button, { label: 'Go', onInvoke: () => (latest = 'second') })));

  await stage.push({ event: { Invoke: { node: 1, id: '1:Invoke' } } });
  await until(() => latest === 'second');
  assert.equal(stage.calls.length, 2, 'rebinding a closure is not something the host needs to hear about');

  session.close();
  stage.close();
});

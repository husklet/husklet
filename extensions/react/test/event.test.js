import assert from 'node:assert/strict';
import net from 'node:net';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { createElement as h } from 'react';

import { connect, render, useHostEvents, usePaneSelection } from '../src/index.js';
import { Button, Column, Text } from '../src/components.js';
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
  let slot = 0;
  const server = net.createServer((stream) => {
    accepted = stream;
    const reader = new Reader();
    stream.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.kind === KIND.request && frame.channel !== 0) {
          calls.push(frame.payload);
          const payload = ['interface_open_tab', 'interface_split'].includes(frame.payload.call)
            ? { reply: 'identity', with: `surface-${slot += 1}` }
            : { reply: 'done' };
          stream.write(encode({ channel: frame.channel, kind: KIND.response, payload }));
        }
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

test('a tab identity addresses every frame rendered into it', async () => {
  const stage = await host();
  const session = await connect({ path: stage.socket });
  render(h(Column, null, h(Button, { label: 'Go', onInvoke: () => {} })), session, { title: 'Demo' });
  await until(() => stage.calls.length >= 2);
  assert.deepEqual(stage.calls[0], { call: 'interface_open_tab', with: { title: 'Demo' } });
  assert.equal(stage.calls[1].call, 'interface_render_at');
  assert.equal(stage.calls[1].with.slot, 'surface-1');
  assert.equal(stage.calls[1].with.frame.sequence, 1);
  session.close();
  stage.close();
});

test('two roots keep independent slots, sequences, sources, and events over one socket', async () => {
  const stage = await host();
  const session = await connect({ path: stage.socket });
  let firstInvoked = 0;
  let secondInvoked = 0;
  const first = render(h(Button, { label: 'First', onInvoke: () => (firstInvoked += 1) }), session, { title: 'First' });
  const second = render(h(Button, { label: 'Second', onInvoke: () => (secondInvoked += 1) }), session, {
    split: { slot: 'surface-1', division: 'beside' },
  });
  assert.deepEqual(await Promise.all([first.ready, second.ready]), ['surface-1', 'surface-2']);
  assert.deepEqual(stage.calls[1], {
    call: 'interface_split',
    with: { slot: 'surface-1', division: 'beside' },
  });
  await until(() => stage.calls.filter((call) => call.call === 'interface_render_at').length === 2);
  const renders = stage.calls.filter((call) => call.call === 'interface_render_at');
  assert.deepEqual(renders.map((call) => [call.with.slot, call.with.frame.sequence]), [
    ['surface-1', 1],
    ['surface-2', 1],
  ]);

  await second.source({ Length: { source: 7, version: 2, rows: 100_000 } });
  assert.deepEqual(stage.calls.at(-1), {
    call: 'source_resize_at',
    with: { slot: 'surface-2', mutation: { Length: { source: 7, version: 2, rows: 100_000 } } },
  });
  await stage.push({ slot: 'surface-2', event: 'Invoke', id: '1:Invoke', node: 1 });
  await until(() => secondInvoked === 1);
  assert.equal(firstInvoked, 0, 'an addressed event never fans out to the other root');
  const closing = first.close();
  assert.equal(first.close(), closing, 'closing is idempotent even while withdrawal is in flight');
  await closing;
  assert.deepEqual(
    stage.calls.filter((call) => call.call === 'interface_withdraw'),
    [{ call: 'interface_withdraw', with: { slot: 'surface-1' } }],
  );
  await stage.push({ slot: 'surface-2', event: 'Invoke', id: '1:Invoke', node: 1 });
  await until(() => secondInvoked === 2);
  assert.equal(firstInvoked, 0, 'withdrawing one root leaves its sibling live');
  await second.close();
  session.close();
  stage.close();
});

test('the client refuses a thirty-third live root before opening it', async () => {
  const stage = await host();
  const session = await connect({ path: stage.socket });
  const roots = Array.from({ length: 32 }, (_, index) => render(h(Button, { label: `${index}` }), session));
  assert.throws(() => render(h(Button, { label: 'overflow' }), session), /surface limit of 32/);
  await Promise.all(roots.map((root) => root.ready));
  assert.equal(stage.calls.filter((call) => call.call === 'interface_open_tab').length, 32);
  await Promise.all(roots.map((root) => root.close()));
  session.close();
  stage.close();
});

test('closing before the open reply withdraws after readiness without rendering', async () => {
  const stage = await host();
  const session = await connect({ path: stage.socket });
  const root = render(h(Button, { label: 'Short lived' }), session);
  const closing = root.close();
  await closing;
  assert.deepEqual(stage.calls, [
    { call: 'interface_open_tab', with: { title: 'Extension' } },
    { call: 'interface_withdraw', with: { slot: 'surface-1' } },
  ]);
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

test('bounded keyboard, focus and pointer details reach their React handlers', async () => {
  const stage = await host();
  const session = await connect({ path: stage.socket });
  const seen = [];
  render(h(Button, {
    label: 'Input target',
    onKey: (event) => seen.push(event),
    onFocus: (event) => seen.push(event),
    onPointer: (event) => seen.push(event),
  }), session, { title: 'Events' });
  await until(() => stage.calls.length >= 2);
  await stage.push({ interaction: 'key', trigger: 'Key', node: 1, id: '1:Key', key: 'a', keycode: 38, modifiers: 4, pressed: true });
  await stage.push({ interaction: 'focus', trigger: 'Focus', node: 1, id: '1:Focus', focused: true });
  await stage.push({ interaction: 'pointer', trigger: 'Pointer', node: 1, id: '1:Pointer', phase: 'motion', x: 2, y: 3, button: 0, modifiers: 0 });
  await until(() => seen.length === 3);
  assert.deepEqual(seen.map(({ trigger }) => trigger), ['Key', 'Focus', 'Pointer']);
  assert.equal(seen[0].key, 'a');
  assert.equal(seen[1].focused, true);
  assert.deepEqual([seen[2].x, seen[2].y], [2, 3]);
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

test('the host-event hook keeps a fresh callback and disposes on unmount', async () => {
  const stage = await host();
  const session = await connect({ path: stage.socket });
  const seen = [];
  function Observer({ name }) {
    useHostEvents(session, () => seen.push(name));
    return h(Text, { label: name });
  }
  const handle = render(h(Observer, { name: 'first' }), session, { title: 'Observer' });
  await handle.ready;
  await until(() => stage.calls.some((call) => call.call === 'interface_render_at'));
  await stage.push({ pane_provider: 'logs', slot: 'pane-1' });
  await until(() => seen.length === 1);

  handle.update(h(Observer, { name: 'second' }));
  await stage.push({ pane_provider: 'logs', slot: 'pane-2' });
  await until(() => seen.length === 2);
  assert.deepEqual(seen, ['first', 'second'], 'a re-render retained the stale listener closure');

  await handle.close();
  await stage.push({ pane_provider: 'logs', slot: 'pane-3' });
  await new Promise((resolve) => setTimeout(resolve, 20));
  assert.deepEqual(seen, ['first', 'second'], 'an unmounted hook remained subscribed');
  session.close();
  stage.close();
});

test('the pane-selection hook filters providers and exposes stable slot identity', async () => {
  const stage = await host();
  const session = await connect({ path: stage.socket });
  function Selection() {
    const selection = usePaneSelection(session, 'logs');
    return h(Text, { label: selection === null ? 'No logs pane selected' : `Logs in ${selection.slot}` });
  }
  const handle = render(h(Selection), session, { title: 'Provider' });
  await handle.ready;
  await until(() => stage.calls.some((call) => call.call === 'interface_render_at'));
  const labels = () => stage.calls
    .filter((call) => call.call === 'interface_render_at')
    .flatMap((call) => call.with.frame.patches)
    .filter((patch) => 'SetProp' in patch && patch.SetProp.prop === 'Label')
    .map((patch) => patch.SetProp.value.Text);

  await stage.push({ pane_provider: 'logs' });
  await stage.push({ pane_provider: 'images', slot: 'pane-wrong' });
  await new Promise((resolve) => setTimeout(resolve, 20));
  assert.equal(labels().includes('Logs in pane-wrong'), false, 'a different provider changed the selected view');
  await stage.push({ pane_provider: 'logs', slot: 'pane-7' });
  await until(() => labels().includes('Logs in pane-7'));

  await handle.close();
  session.close();
  stage.close();
});

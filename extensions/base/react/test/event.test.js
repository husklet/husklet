import assert from 'node:assert/strict';
import net from 'node:net';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { createElement as h } from 'react';

import { connect, render, useHostEvents, usePaneSelection } from '../dist/index.js';
import { Button, Column, Container, DataTable, Text } from '../dist/components.js';
import { KIND, Reader, encode } from '../dist/wire.js';
import { PROTOCOL } from '../dist/session.js';

/** A host that greets, records calls, and can push an event back. */
async function host({ reuseSlots = false, rejectFirstRender = false } = {}) {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'husklet-react-'));
  const socket = path.join(directory, 'extension.sock');
  const calls = [];
  const answers = [];
  let connected;
  const arrived = new Promise((resolve) => {
    connected = resolve;
  });
  let accepted = null;
  let slot = 0;
  let eventChannel = 1;
  let rejectedRender = false;
  const server = net.createServer((stream) => {
    accepted = stream;
    const reader = new Reader();
    stream.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.kind === KIND.response && frame.channel !== 0) {
          answers.push({ channel: frame.channel, window: frame.payload });
          continue;
        }
        if (frame.kind === KIND.request && frame.channel !== 0) {
          calls.push(frame.payload);
          const payload = ['interface_open_tab', 'interface_split'].includes(frame.payload.call)
            ? { reply: 'identity', with: reuseSlots ? 'surface-1' : `surface-${(slot += 1)}` }
            : { reply: 'done' };
          if (
            rejectFirstRender &&
            frame.payload.call === 'interface_render_at' &&
            !rejectedRender
          ) {
            rejectedRender = true;
            setTimeout(
              () =>
                stream.write(
                  encode({
                    channel: frame.channel,
                    kind: KIND.response,
                    flags: 3,
                    payload: { error: 'failed', detail: 'first frame rejected' },
                  }),
                ),
              20,
            );
          } else {
            stream.write(encode({ channel: frame.channel, kind: KIND.response, payload }));
          }
        }
      }
    });
    stream.write(
      encode({
        channel: 0,
        kind: KIND.open,
        payload: { protocol: PROTOCOL, extension: 'demo', granted: ['interface:render'] },
      }),
    );
    connected(stream);
  });
  await new Promise((resolve) => server.listen(socket, resolve));
  return {
    socket,
    calls,
    answers,
    stream: () => arrived,
    async push(payload) {
      eventChannel += 1;
      (await arrived).write(encode({ channel: eventChannel, kind: KIND.event, payload }));
    },
    async pushFragmented(payload) {
      eventChannel += 1;
      const frame = encode({ channel: eventChannel, kind: KIND.event, payload });
      const stream = await arrived;
      for (const byte of frame) stream.write(Buffer.of(byte));
    },
    close() {
      accepted?.destroy();
      server.close();
      fs.rmSync(directory, { recursive: true, force: true });
    },
  };
}

test('a rejected frame stops later surface frames before they cross the Unix socket', async () => {
  const stage = await host({ rejectFirstRender: true });
  const session = await connect({ path: stage.socket });
  const handle = render(h(Text, { label: 'first' }), session, { title: 'Ordered' });
  await handle.ready;
  handle.update(h(Text, { label: 'second' }));
  await assert.rejects(handle.flush(), /first frame rejected/);
  await new Promise((resolve) => setTimeout(resolve, 30));
  const renders = stage.calls.filter(({ call }) => call === 'interface_render_at');
  assert.equal(renders.length, 1);
  assert.equal(renders[0].with.frame.sequence, 1);
  await session.close();
  stage.close();
});

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
  render(h(Column, null, h(Button, { label: 'Go', onInvoke: () => {} })), session, {
    title: 'Demo',
  });
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
  const first = render(
    h(Button, { label: 'First', onInvoke: () => (firstInvoked += 1) }),
    session,
    { title: 'First' },
  );
  const second = render(
    h(Button, { label: 'Second', onInvoke: () => (secondInvoked += 1) }),
    session,
    {
      split: { slot: 'surface-1', division: 'beside' },
    },
  );
  assert.deepEqual(await Promise.all([first.ready, second.ready]), ['surface-1', 'surface-2']);
  assert.equal(first.slot, 'surface-1');
  assert.equal(second.slot, 'surface-2');
  assert.deepEqual(stage.calls[1], {
    call: 'interface_split',
    with: { slot: 'surface-1', division: 'beside' },
  });
  await until(() => stage.calls.filter((call) => call.call === 'interface_render_at').length === 2);
  const renders = stage.calls.filter((call) => call.call === 'interface_render_at');
  assert.deepEqual(
    renders.map((call) => [call.with.slot, call.with.frame.sequence]),
    [
      ['surface-1', 1],
      ['surface-2', 1],
    ],
  );

  await second.source({ Length: { source: 7, version: 2, rows: 100_000 } });
  assert.deepEqual(stage.calls.at(-1), {
    call: 'source_resize_at',
    with: { slot: 'surface-2', mutation: { Length: { source: 7, version: 2, rows: 100_000 } } },
  });
  await second.source({
    Open: {
      source: 7,
      columns: [{ key: 'pid', title: 'PID', width: { chars: 8 }, sortable: true }],
    },
  });
  assert.deepEqual(stage.calls.at(-1), {
    call: 'source_resize_at',
    with: {
      slot: 'surface-2',
      mutation: {
        Open: {
          source: 7,
          columns: [
            {
              key: 'pid',
              title: 'PID',
              width: { Chars: 8 },
              align: 'Start',
              sortable: true,
              editable: false,
              importance: 'Essential',
              identity: false,
            },
          ],
        },
      },
    },
  });
  await stage.push({
    interaction: 'invoke',
    trigger: 'Invoke',
    slot: 'surface-2',
    id: '1:Invoke',
    node: 1,
  });
  await until(() => secondInvoked === 1);
  assert.equal(firstInvoked, 0, 'an addressed event never fans out to the other root');
  const closing = first.close();
  assert.equal(first.close(), closing, 'closing is idempotent even while withdrawal is in flight');
  await closing;
  assert.deepEqual(
    stage.calls.filter((call) => call.call === 'interface_withdraw'),
    [{ call: 'interface_withdraw', with: { slot: 'surface-1' } }],
  );
  await stage.push({
    interaction: 'invoke',
    trigger: 'Invoke',
    slot: 'surface-2',
    id: '1:Invoke',
    node: 1,
  });
  await until(() => secondInvoked === 2);
  assert.equal(firstInvoked, 0, 'withdrawing one root leaves its sibling live');
  await second.close();
  session.close();
  stage.close();
});

test('a withdrawn slot cannot be rebound to a new render generation on the same socket', async () => {
  const stage = await host({ reuseSlots: true });
  const session = await connect({ path: stage.socket });
  let successor;
  let oldInvoked = 0;
  let newInvoked = 0;
  const first = render(h(Button, { label: 'Old', onInvoke: () => (oldInvoked += 1) }), session);
  try {
    assert.equal(await first.ready, 'surface-1');
    await first.flush();
    await first.close();

    const second = render(h(Button, { label: 'New', onInvoke: () => (newInvoked += 1) }), session);
    await assert.rejects(second.ready, /reused surface slot surface-1 within one session/);
    await stage.push({
      interaction: 'invoke',
      trigger: 'Invoke',
      slot: 'surface-1',
      id: '1:Invoke',
      node: 1,
    });
    await new Promise((resolve) => setTimeout(resolve, 20));
    assert.equal(oldInvoked, 0, 'withdrawn handlers stay revoked');
    assert.equal(newInvoked, 0, 'a delayed event cannot enter a rejected render generation');

    await session.close();
    successor = await connect({ path: stage.socket });
    const third = render(h(Button, { label: 'Fresh connection' }), successor);
    assert.equal(
      await third.ready,
      'surface-1',
      'slot retirement ends at the socket generation boundary',
    );
    await third.close();
  } finally {
    await session.close();
    await successor?.close();
    stage.close();
  }
});

test('virtualized row requests route to their owning surface with the exact cursor', async () => {
  const stage = await host();
  const errors = [];
  const session = await connect({
    path: stage.socket,
    onEventError: (error) => errors.push(error),
  });
  const firstRequests = [];
  const secondRequests = [];
  const first = render(h(DataTable, { source: 9, schema: [] }), session, {
    title: 'First',
    rows: (request) => {
      firstRequests.push(request);
      return [];
    },
  });
  const second = render(h(DataTable, { source: 9, schema: [] }), session, {
    split: { slot: 'surface-1', division: 'beside' },
    rows: async (request) => {
      secondRequests.push(request);
      return [{ key: request.range.start, cells: [{ Text: 'millionth row' }] }];
    },
  });
  await Promise.all([first.ready, second.ready]);
  await stage.push({
    id: 71,
    source: 9,
    version: 4,
    range: { start: 999_936, count: 128 },
    sort: { column: 'name', descending: true },
    filter: 'active',
    slot: 'surface-2',
  });
  await until(() => stage.answers.length === 1);
  assert.equal(firstRequests.length, 0);
  assert.equal(secondRequests.length, 1);
  assert.deepEqual(stage.answers[0], {
    channel: 2,
    window: {
      source: 9,
      version: 4,
      request: 71,
      range: { start: 999_936, count: 128 },
      rows: [{ key: 999_936, cells: [{ Text: 'millionth row' }] }],
    },
  });
  assert.deepEqual(errors, []);
  await Promise.all([first.close(), second.close()]);
  await session.close();
  stage.close();
});

test('published source version refuses unannounced Postgres windows without running SQL', async () => {
  const stage = await host();
  const errors = [];
  const requests = [];
  const session = await connect({
    path: stage.socket,
    onEventError: (error) => errors.push(error),
  });
  const surface = render(h(DataTable, { source: 17, schema: [] }), session, {
    title: 'Postgres million-row browser',
    rows: (request) => {
      requests.push(request);
      return [{ key: request.range.start, cells: [{ Text: 'database row' }] }];
    },
  });
  await surface.ready;
  await surface.source({ Length: { source: 17, version: 4, rows: 1_000_000 } });

  const window = {
    source: 17,
    range: { start: 999_936, count: 64 },
    sort: null,
    filter: null,
    slot: surface.slot,
  };
  await stage.pushFragmented({ ...window, id: 80, version: 5 });
  await until(() => stage.answers.length === 1);
  assert.equal(requests.length, 0, 'an unannounced generation never reaches database code');
  assert.deepEqual(stage.answers[0].window, {
    source: 17,
    version: 5,
    request: 80,
    range: window.range,
    rows: [],
  });

  await stage.pushFragmented({ ...window, id: 81, version: 4 });
  await until(() => stage.answers.length === 2);
  assert.equal(requests.length, 1, 'the exact published generation remains usable');
  assert.equal(stage.answers[1].window.request, 81);
  assert.deepEqual(stage.answers[1].window.rows, [
    { key: 999_936, cells: [{ Text: 'database row' }] },
  ]);
  let staleSignal;
  surface.setRowProvider((_request, { signal }) => {
    staleSignal = signal;
    return new Promise(() => {});
  });
  await stage.pushFragmented({ ...window, id: 82, version: 4 });
  await until(() => staleSignal !== undefined);
  await surface.source({ Invalidate: { source: 17, version: 6 } });
  assert.equal(staleSignal.aborted, true, 'publishing a database snapshot cancels old SQL');
  await until(() => stage.answers.length === 3);
  assert.equal(stage.answers[2].window.request, 82);
  assert.deepEqual(stage.answers[2].window.rows, []);

  surface.setRowProvider((request) => [
    { key: request.range.start, cells: [{ Text: 'replacement snapshot' }] },
  ]);
  await stage.pushFragmented({ ...window, id: 83, version: 6 });
  await until(() => stage.answers.length === 4);
  assert.deepEqual(stage.answers[3].window.rows, [
    { key: 999_936, cells: [{ Text: 'replacement snapshot' }] },
  ]);
  assert.deepEqual(errors, []);
  await surface.close();
  await session.close();
  stage.close();
});

test('row provider rejection is reported and the surface still closes cleanly', async () => {
  const stage = await host();
  const errors = [];
  const session = await connect({
    path: stage.socket,
    onEventError: (error) => errors.push(error),
  });
  const surface = render(h(DataTable, { source: 3, schema: [] }), session, {
    rows: () => Array.from({ length: 129 }, (_, key) => ({ key, cells: [] })),
  });
  await surface.ready;
  await stage.push({
    id: 1,
    source: 3,
    version: 1,
    range: { start: 0, count: 128 },
    sort: null,
    filter: null,
    slot: surface.slot,
  });
  await until(() => errors.length === 1);
  assert.match(String(errors[0]), /more rows than the requested window/);
  await until(() => stage.answers.length === 1);
  assert.deepEqual(stage.answers[0].window.rows, []);
  await surface.close();
  assert.equal(stage.calls.at(-1).call, 'interface_withdraw');
  await session.close();
  stage.close();
});

test('closing a surface discards its in-flight row provider result', async () => {
  const stage = await host();
  const session = await connect({ path: stage.socket });
  let release;
  let signal;
  const pending = new Promise((resolve) => {
    release = resolve;
  });
  const surface = render(h(DataTable, { source: 3, schema: [] }), session, {
    rows: (_request, context) => {
      signal = context.signal;
      return pending;
    },
  });
  await surface.ready;
  await stage.push({
    id: 1,
    source: 3,
    version: 1,
    range: { start: 0, count: 128 },
    sort: null,
    filter: null,
    slot: surface.slot,
  });
  await until(() => signal !== undefined);
  await surface.close();
  assert.equal(signal.aborted, true, 'closing cooperatively cancels database/file work');
  release([{ key: 0, cells: [] }]);
  await new Promise((resolve) => setTimeout(resolve, 20));
  assert.deepEqual(stage.answers[0].window.rows, []);
  await session.close();
  stage.close();
});

test('row providers bound concurrent work under a rapid million-row scroll', async () => {
  const stage = await host();
  const session = await connect({ path: stage.socket });
  const signals = [];
  const surface = render(h(DataTable, { source: 3, schema: [] }), session, {
    rows: (_request, { signal }) => {
      signals.push(signal);
      return new Promise(() => {});
    },
  });
  await surface.ready;
  for (let id = 1; id <= 40; id += 1) {
    await stage.push({
      id,
      source: 3,
      version: 1,
      range: { start: id * 128, count: 128 },
      sort: null,
      filter: null,
      slot: surface.slot,
    });
  }
  await until(() => signals.length === 4);
  await new Promise((resolve) => setTimeout(resolve, 20));
  assert.equal(signals.length, 4, 'only the documented concurrency reaches extension code');
  await surface.close();
  assert.equal(
    signals.every((signal) => signal.aborted),
    true,
  );
  await session.close();
  stage.close();
});

test('a newer request cancels obsolete work for the same row window', async () => {
  const stage = await host();
  const session = await connect({ path: stage.socket });
  const seen = [];
  const surface = render(h(DataTable, { source: 3, schema: [] }), session, {
    rows: (request, { signal }) => {
      seen.push({ request, signal });
      return new Promise(() => {});
    },
  });
  await surface.ready;
  const request = {
    source: 3,
    version: 1,
    range: { start: 999_936, count: 128 },
    sort: null,
    filter: null,
    slot: surface.slot,
  };
  await stage.push({ ...request, id: 1 });
  await until(() => seen.length === 1);
  await stage.push({ ...request, id: 2 });
  await until(() => seen.length === 2);
  assert.equal(seen[0].signal.aborted, true);
  assert.equal(seen[1].signal.aborted, false);
  await surface.close();
  await session.close();
  stage.close();
});

test('a newer source version cancels every stale range and rejects late stale requests', async () => {
  const stage = await host();
  const session = await connect({ path: stage.socket });
  const seen = [];
  const surface = render(h(DataTable, { source: 3, schema: [] }), session, {
    rows: (request, { signal }) => {
      seen.push({ request, signal });
      if (request.version === 2) {
        return [{ key: request.range.start, cells: [{ Text: 'current' }] }];
      }
      return new Promise((_resolve, reject) => {
        signal.addEventListener('abort', () => reject(signal.reason), { once: true });
      });
    },
  });
  await surface.ready;
  for (let id = 1; id <= 5; id += 1) {
    await stage.push({
      id,
      source: 3,
      version: 1,
      range: { start: id * 128, count: 128 },
      sort: null,
      filter: null,
      slot: surface.slot,
    });
  }
  await until(() => seen.length === 4);

  await stage.push({
    id: 6,
    source: 3,
    version: 2,
    range: { start: 0, count: 128 },
    sort: null,
    filter: null,
    slot: surface.slot,
  });
  await until(() => seen.some(({ request }) => request.version === 2));
  assert.equal(
    seen.filter(({ request }) => request.version === 1).length,
    4,
    'the queued stale range never reaches extension code',
  );
  assert.equal(
    seen.filter(({ request }) => request.version === 1).every(({ signal }) => signal.aborted),
    true,
  );
  await until(() => stage.answers.some(({ window }) => window.version === 2));

  await stage.push({
    id: 7,
    source: 3,
    version: 1,
    range: { start: 256, count: 128 },
    sort: null,
    filter: null,
    slot: surface.slot,
  });
  await new Promise((resolve) => setTimeout(resolve, 20));
  assert.equal(seen.length, 5, 'a delayed stale host request is consumed without querying');

  await surface.source({ Length: { source: 3, version: 3, rows: 0 } });
  assert.equal(
    stage.calls.some(
      (call) => call.call === 'source_resize_at' && call.with.mutation.Length?.version === 3,
    ),
    true,
    'the same Unix session remains usable after cancellation and stale input',
  );
  await surface.close();
  await session.close();
  stage.close();
});

test('replacing a row provider cancels stale query work without blocking the new query', async () => {
  const stage = await host();
  const session = await connect({ path: stage.socket });
  const staleSignals = [];
  const freshRequests = [];
  const surface = render(h(DataTable, { source: 3, schema: [] }), session, {
    rows: (_request, { signal }) => {
      staleSignals.push(signal);
      return new Promise(() => {});
    },
  });
  await surface.ready;
  for (let id = 1; id <= 5; id += 1) {
    await stage.push({
      id,
      source: 3,
      version: 1,
      range: { start: id * 128, count: 128 },
      sort: null,
      filter: null,
      slot: surface.slot,
    });
  }
  await until(() => staleSignals.length === 4);

  surface.setRowProvider((request) => {
    freshRequests.push(request);
    return [{ key: request.range.start, cells: [{ Text: 'fresh query' }] }];
  });
  assert.equal(
    staleSignals.every((signal) => signal.aborted),
    true,
  );
  await stage.push({
    id: 6,
    source: 3,
    version: 2,
    range: { start: 0, count: 128 },
    sort: null,
    filter: null,
    slot: surface.slot,
  });
  await until(() => freshRequests.length === 1);
  await until(() => stage.answers.some(({ window }) => window.version === 2));
  assert.equal(freshRequests[0].version, 2);
  assert.equal(staleSignals.length, 4, 'the queued old window never reaches either provider');

  await surface.close();
  await session.close();
  stage.close();
});

test('a slotless event never guesses between multiple surfaces', async () => {
  const stage = await host();
  const session = await connect({ path: stage.socket });
  let firstInvoked = 0;
  let secondInvoked = 0;
  const first = render(
    h(Button, { label: 'First', onInvoke: () => (firstInvoked += 1) }),
    session,
    { title: 'First' },
  );
  const second = render(
    h(Button, { label: 'Second', onInvoke: () => (secondInvoked += 1) }),
    session,
    {
      split: { slot: 'surface-1', division: 'beside' },
    },
  );
  await Promise.all([first.ready, second.ready]);
  await until(() => stage.calls.filter((call) => call.call === 'interface_render_at').length === 2);
  await stage.push({ interaction: 'invoke', trigger: 'Invoke', node: 1, id: '1:Invoke' });
  await new Promise((resolve) => setTimeout(resolve, 20));
  assert.deepEqual([firstInvoked, secondInvoked], [0, 0]);
  await stage.push({
    interaction: 'invoke',
    trigger: 'Invoke',
    node: 1,
    id: '1:Invoke',
    slot: 'surface-2',
  });
  await until(() => secondInvoked === 1);
  assert.equal(firstInvoked, 0);
  await Promise.all([first.close(), second.close()]);
  session.close();
  stage.close();
});

test('the client refuses a thirty-third live root before opening it', async () => {
  const stage = await host();
  const session = await connect({ path: stage.socket });
  const roots = Array.from({ length: 32 }, (_, index) =>
    render(h(Button, { label: `${index}` }), session),
  );
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
  render(h(Column, null, h(Button, { label: 'Go', onInvoke: () => (invoked += 1) })), session, {
    title: 'Demo',
  });
  await until(() => stage.calls.length >= 2);

  await stage.push({
    interaction: 'invoke',
    trigger: 'Invoke',
    node: 1,
    id: '1:Invoke',
    slot: 'surface-1',
  });
  await until(() => invoked === 1);

  session.close();
  stage.close();
});

test('bounded keyboard, focus and pointer details reach their React handlers', async () => {
  const stage = await host();
  const session = await connect({ path: stage.socket });
  const seen = [];
  render(
    h(Button, {
      label: 'Input target',
      onKey: (event) => seen.push(event),
      onFocus: (event) => seen.push(event),
      onPointer: (event) => seen.push(event),
    }),
    session,
    { title: 'Events' },
  );
  await until(() => stage.calls.length >= 2);
  await stage.push({
    interaction: 'key',
    trigger: 'Key',
    node: 1,
    id: '1:Key',
    key: 'a',
    keycode: 38,
    modifiers: 4,
    pressed: true,
  });
  await stage.push({
    interaction: 'focus',
    trigger: 'Focus',
    node: 1,
    id: '1:Focus',
    focused: true,
  });
  await stage.push({
    interaction: 'pointer',
    trigger: 'Pointer',
    node: 1,
    id: '1:Pointer',
    phase: 'motion',
    x: 2,
    y: 3,
    button: 0,
    modifiers: 0,
  });
  await until(() => seen.length === 3);
  assert.deepEqual(
    seen.map(({ trigger }) => trigger),
    ['Key', 'Focus', 'Pointer'],
  );
  assert.equal(seen[0].key, 'a');
  assert.equal(seen[1].focused, true);
  assert.deepEqual([seen[2].x, seen[2].y], [2, 3]);
  session.close();
  stage.close();
});

test('a version-bound virtual row edit reaches only its DataTable handler', async () => {
  const stage = await host();
  const session = await connect({ path: stage.socket });
  let seen = null;
  render(
    h(DataTable, {
      source: 7,
      schema: [{ key: 'name', editable: true }],
      onEdit: (event) => {
        seen = event;
      },
    }),
    session,
  );
  await until(() => stage.calls.length >= 2);
  const patches = stage.calls.find(({ call }) => call === 'interface_render_at').with.frame.patches;
  const edit = patches.find((patch) => patch.SetHandler?.handler.trigger === 'Edit').SetHandler;
  await stage.push({
    interaction: 'edit',
    trigger: 'Edit',
    node: edit.id,
    id: edit.handler.id,
    source: 7,
    version: 4,
    row: { index: 9, id: 'immutable-9' },
    column: 'name',
    value: 'renamed',
  });
  await until(() => seen !== null);
  assert.deepEqual(
    {
      source: seen.source,
      version: seen.version,
      row: seen.row,
      column: seen.column,
      value: seen.value,
    },
    {
      source: 7,
      version: 4,
      row: { index: 9, id: 'immutable-9' },
      column: 'name',
      value: 'renamed',
    },
  );
  session.close();
  stage.close();
});

test('a version-bound native sort reaches only its DataTable handler', async () => {
  const stage = await host();
  const session = await connect({ path: stage.socket });
  let seen = null;
  render(
    h(DataTable, {
      source: 7,
      schema: [{ key: 'name', sortable: true }],
      onSort: (event) => {
        seen = event;
      },
    }),
    session,
  );
  await until(() => stage.calls.length >= 2);
  const patches = stage.calls.find(({ call }) => call === 'interface_render_at').with.frame.patches;
  const sort = patches.find((patch) => patch.SetHandler?.handler.trigger === 'Sort').SetHandler;
  await stage.push({
    interaction: 'sort',
    trigger: 'Sort',
    node: sort.id,
    id: sort.handler.id,
    source: 7,
    version: 4,
    column: 'name',
    descending: true,
  });
  await until(() => seen !== null);
  assert.deepEqual(
    {
      source: seen.source,
      version: seen.version,
      column: seen.column,
      descending: seen.descending,
    },
    {
      source: 7,
      version: 4,
      column: 'name',
      descending: true,
    },
  );
  session.close();
  stage.close();
});

test('bounded internal drag and drop metadata reaches the exact React handlers', async () => {
  const stage = await host();
  const session = await connect({ path: stage.socket });
  const seen = [];
  render(
    h(
      Column,
      null,
      h(Container, {
        onDrag: (event) => seen.push(event),
        onDrop: (event) => seen.push(event),
      }),
    ),
    session,
    { title: 'Drag and drop' },
  );
  await until(() => stage.calls.length >= 2);
  const patches = stage.calls.find(({ call }) => call === 'interface_render_at').with.frame.patches;
  const drag = patches.find((patch) => patch.SetHandler?.handler.trigger === 'Drag').SetHandler;
  const drop = patches.find((patch) => patch.SetHandler?.handler.trigger === 'Drop').SetHandler;
  await stage.push({
    interaction: 'drag',
    trigger: 'Drag',
    node: drag.id,
    id: drag.handler.id,
    slot: 'surface-1',
  });
  await stage.push({
    interaction: 'drop',
    trigger: 'Drop',
    node: drop.id,
    id: drop.handler.id,
    slot: 'surface-1',
    source: 4,
    x: 2,
    y: 3,
  });
  await until(() => seen.length === 2);
  assert.deepEqual(
    seen.map(({ trigger }) => trigger),
    ['Drag', 'Drop'],
  );
  assert.equal(seen[0].slot, 'surface-1');
  assert.deepEqual([seen[1].source, seen[1].x, seen[1].y], [4, 2, 3]);
  session.close();
  stage.close();
});

test('a re-render rebinds the callback without a patch', async () => {
  const stage = await host();
  const session = await connect({ path: stage.socket });
  let latest = 'first';
  const handle = render(
    h(Column, null, h(Button, { label: 'Go', onInvoke: () => (latest = 'first') })),
    session,
    {
      title: 'Demo',
    },
  );
  await until(() => stage.calls.length >= 2);
  handle.update(h(Column, null, h(Button, { label: 'Go', onInvoke: () => (latest = 'second') })));

  await stage.push({
    interaction: 'invoke',
    trigger: 'Invoke',
    node: 1,
    id: '1:Invoke',
    slot: 'surface-1',
  });
  await until(() => latest === 'second');
  assert.equal(
    stage.calls.length,
    2,
    'rebinding a closure is not something the host needs to hear about',
  );

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

test('reattaching the same callback cannot replay a queued event from its old generation', async () => {
  const stage = await host();
  const session = await connect({ path: stage.socket });
  const seen = [];
  let release;
  let entered;
  const firstEntered = new Promise((resolve) => {
    entered = resolve;
  });
  const listener = async (event) => {
    seen.push(event.slot);
    if (event.slot === 'old-1') {
      entered();
      await new Promise((resolve) => {
        release = resolve;
      });
    }
  };
  const dispose = session.onEvent(listener);
  try {
    await stage.push({ pane_provider: 'logs', slot: 'old-1' });
    await firstEntered;
    await stage.push({ pane_provider: 'logs', slot: 'old-2' });
    await new Promise((resolve) => setTimeout(resolve, 20));
    dispose();
    const disposeFresh = session.onEvent(listener);
    release();
    await new Promise((resolve) => setTimeout(resolve, 20));
    assert.deepEqual(seen, ['old-1'], 'queued work crossed the listener generation boundary');

    await stage.push({ pane_provider: 'logs', slot: 'fresh' });
    await until(() => seen.length === 2);
    assert.deepEqual(seen, ['old-1', 'fresh']);
    disposeFresh();
  } finally {
    release?.();
    await session.close();
    stage.close();
  }
});

test('the pane-selection hook filters providers and exposes stable slot identity', async () => {
  const stage = await host();
  const session = await connect({ path: stage.socket });
  function Selection() {
    const selection = usePaneSelection(session, 'logs');
    return h(Text, {
      label: selection === null ? 'No logs pane selected' : `Logs in ${selection.slot}`,
    });
  }
  const handle = render(h(Selection), session, { title: 'Provider' });
  await handle.ready;
  await until(() => stage.calls.some((call) => call.call === 'interface_render_at'));
  const labels = () =>
    stage.calls
      .filter((call) => call.call === 'interface_render_at')
      .flatMap((call) => call.with.frame.patches)
      .filter((patch) => 'SetProp' in patch && patch.SetProp.prop === 'Label')
      .map((patch) => patch.SetProp.value.Text);

  await stage.push({ pane_provider: 'images', slot: 'pane-wrong' });
  await new Promise((resolve) => setTimeout(resolve, 20));
  assert.equal(
    labels().includes('Logs in pane-wrong'),
    false,
    'a different provider changed the selected view',
  );
  await stage.push({ pane_provider: 'logs', slot: 'pane-7' });
  await until(() => labels().includes('Logs in pane-7'));

  await handle.close();
  session.close();
  stage.close();
});

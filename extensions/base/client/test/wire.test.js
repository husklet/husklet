import assert from 'node:assert/strict';
import net from 'node:net';
import os from 'node:os';
import path from 'node:path';
import { mkdtemp, rm } from 'node:fs/promises';
import { once } from 'node:events';
import test from 'node:test';

import { CONTROL, FLAG_END, HEADER, KIND, PAYLOAD_LIMIT, Reader, encode } from '../dist/wire.js';

test('a frame survives the codec unchanged', () => {
  const frame = {
    sequence: 3,
    patches: [
      { Create: { id: 1, tag: 'Card' } },
      { Insert: { parent: 0, child: 1, before: null } },
      { SetProp: { id: 1, prop: 'Label', value: { Text: 'Go' } } },
    ],
  };
  const payload = { call: 'interface_render_at', with: { slot: 'pane-2', frame } };
  const [read] = new Reader().take(encode({ channel: 1, kind: KIND.request, payload }));
  assert.deepEqual(read.payload, payload);
  assert.equal(read.channel, 1);
  assert.equal(read.kind, KIND.request);
  assert.equal(read.flags, FLAG_END);
});

test('idle readers stay small and release a near-limit frame fragmented over Unix', async () => {
  const readers = Array.from({ length: 64 }, () => new Reader());
  assert.equal(
    readers.reduce((total, reader) => total + reader.capacity, 0),
    64 * HEADER,
  );

  const payload = Buffer.alloc(PAYLOAD_LIMIT - 1, 0x61);
  const encoded = encode({ channel: 0, kind: KIND.ping, payload });
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-large-frame-'));
  const socketPath = path.join(directory, 'wire.sock');
  const server = net.createServer(async (socket) => {
    for (let offset = 0; offset < encoded.length; offset += 257) {
      if (!socket.write(encoded.subarray(offset, offset + 257))) await once(socket, 'drain');
    }
    socket.end();
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));
  const reader = readers[0];
  const frames = [];
  try {
    const socket = net.createConnection(socketPath);
    socket.on('data', (chunk) => {
      frames.push(...reader.take(chunk));
      assert(reader.capacity <= HEADER + PAYLOAD_LIMIT);
    });
    await once(socket, 'end');
    reader.finish();
    assert.equal(frames.length, 1);
    assert.deepEqual(frames[0].payload, payload);
    assert.equal(reader.buffered, 0);
    assert.equal(reader.capacity, HEADER);
  } finally {
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('frames split across chunks are reassembled', () => {
  const bytes = encode({
    channel: CONTROL,
    kind: KIND.event,
    payload: { sequence: 1, patches: [] },
  });
  const reader = new Reader();
  assert.deepEqual(reader.take(bytes.subarray(0, 5)), []);
  const [read] = reader.take(bytes.subarray(5));
  assert.deepEqual(read.payload, { sequence: 1, patches: [] });
});

test('header violations are rejected before a declared body is buffered', () => {
  for (const [offset, value, message] of [
    [8, 99, /unknown kind/],
    [9, 0x80, /unknown flags/],
    [10, 1, /reserved/],
  ]) {
    const bytes = encode({ kind: KIND.event, payload: null });
    bytes[offset] = value;
    assert.throws(() => new Reader().take(bytes.subarray(0, HEADER)), message);
  }
  const oversized = Buffer.alloc(HEADER);
  oversized.writeUInt32LE(PAYLOAD_LIMIT + 1);
  oversized.writeUInt8(KIND.event, 8);
  oversized.writeUInt8(FLAG_END, 9);
  assert.throws(() => new Reader().take(oversized), /above the .* limit/);
});

test('EOF rejects every partial frame and accepts a clean boundary', () => {
  const reader = new Reader();
  reader.take(encode({ kind: KIND.event, payload: { ok: true } }).subarray(0, HEADER + 1));
  assert.throws(() => reader.finish(), /unfinished frame/);
  assert.doesNotThrow(() => new Reader().finish());
});

test('control heartbeats retain arbitrary non-JSON bytes', () => {
  const payload = Buffer.from([0, 0xff, 10, 13]);
  const [frame] = new Reader().take(encode({ channel: 19, kind: KIND.ping, payload }));
  assert.deepEqual(frame.payload, payload);
});

test('the encoder rejects values that cannot be represented by the wire header', () => {
  for (const channel of [-1, 0x1_0000_0000, 1.5, Number.NaN]) {
    assert.throws(() => encode({ channel, kind: KIND.event, payload: null }), /unsigned 32-bit/);
  }
  assert.throws(() => encode({ kind: 255, payload: null }), /unknown kind/);
  assert.throws(() => encode({ kind: KIND.event, flags: 0x80, payload: null }), /unknown flags/);
  assert.throws(() => encode({ kind: KIND.event, payload: undefined }), /bytes or a JSON value/);
});

test('one-byte delivery retains a bounded frame without repeated buffer growth', () => {
  const payload = { text: 'x'.repeat(256 * 1024) };
  const bytes = encode({ channel: 9, kind: KIND.event, payload });
  const reader = new Reader();
  let frames = [];
  for (const byte of bytes) {
    frames = reader.take(Uint8Array.of(byte));
    assert(reader.buffered <= HEADER + PAYLOAD_LIMIT);
  }
  assert.deepEqual(frames[0].payload, payload);
  assert.equal(reader.buffered, 0);
});

test('deterministic arbitrary chunk boundaries preserve a mixed frame stream', () => {
  const sent = Array.from({ length: 97 }, (_, index) => ({
    channel: index * 2 + 1,
    kind: index % 11 === 0 ? KIND.ping : KIND.event,
    payload:
      index % 11 === 0
        ? Buffer.from([index, 0, 255 - index])
        : { index, text: 'λ'.repeat((index * 37) % 509), exact: Number.MAX_SAFE_INTEGER - index },
  }));
  const bytes = Buffer.concat(sent.map(encode));
  for (let seed = 1; seed <= 32; seed += 1) {
    const reader = new Reader();
    const received = [];
    let state = seed;
    let offset = 0;
    while (offset < bytes.length) {
      state = (Math.imul(state, 1_664_525) + 1_013_904_223) >>> 0;
      const size = 1 + (state % 4093);
      received.push(...reader.take(bytes.subarray(offset, offset + size)));
      offset += size;
      assert(reader.buffered <= HEADER + PAYLOAD_LIMIT);
    }
    reader.finish();
    assert.deepEqual(
      received.map(({ channel, kind, payload }) => ({ channel, kind, payload })),
      sent,
    );
  }
});

test('malformed JSON at every truncation boundary never yields a partial value', () => {
  const bytes = encode({ channel: 3, kind: KIND.event, payload: { nested: ['✓', { value: 7 }] } });
  for (let cut = HEADER; cut < bytes.length; cut += 1) {
    const reader = new Reader();
    assert.deepEqual(reader.take(bytes.subarray(0, cut)), []);
    assert.throws(() => reader.finish(), /unfinished frame/);
  }
  for (const body of [Buffer.from('{'), Buffer.from('{"n":1e999}'), Buffer.from([0xc3, 0x28])]) {
    const frame = Buffer.alloc(HEADER + body.length);
    frame.writeUInt32LE(body.length);
    frame.writeUInt8(KIND.event, 8);
    frame.writeUInt8(FLAG_END, 9);
    body.copy(frame, HEADER);
    assert.throws(() => new Reader().take(frame), /valid UTF-8 JSON/);
  }
});

test('huge numbers and excessive JSON depth fail before typed dispatch', () => {
  for (const document of ['{"n":1e999}', `{"n":${Number.MAX_SAFE_INTEGER + 2}}`]) {
    const body = Buffer.from(document);
    const frame = Buffer.alloc(HEADER + body.length);
    frame.writeUInt32LE(body.length);
    frame.writeUInt8(KIND.event, 8);
    frame.writeUInt8(FLAG_END, 9);
    body.copy(frame, HEADER);
    assert.throws(() => new Reader().take(frame), /cannot cross the protocol losslessly/);
  }
  const body = Buffer.from(`${'['.repeat(129)}null${']'.repeat(129)}`);
  const frame = Buffer.alloc(HEADER + body.length);
  frame.writeUInt32LE(body.length);
  frame.writeUInt8(KIND.event, 8);
  frame.writeUInt8(FLAG_END, 9);
  body.copy(frame, HEADER);
  assert.throws(() => new Reader().take(frame), /depth limit/);
});

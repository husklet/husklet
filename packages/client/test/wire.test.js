import assert from 'node:assert/strict';
import test from 'node:test';

import { CONTROL, FLAG_END, HEADER, KIND, PAYLOAD_LIMIT, Reader, encode } from '../src/wire.js';

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

test('frames split across chunks are reassembled', () => {
  const bytes = encode({ channel: CONTROL, kind: KIND.event, payload: { sequence: 1, patches: [] } });
  const reader = new Reader();
  assert.deepEqual(reader.take(bytes.subarray(0, 5)), []);
  const [read] = reader.take(bytes.subarray(5));
  assert.deepEqual(read.payload, { sequence: 1, patches: [] });
});

test('header violations are rejected before a declared body is buffered', () => {
  for (const [offset, value, message] of [[8, 99, /unknown kind/], [9, 0x80, /unknown flags/], [10, 1, /reserved/]]) {
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

import assert from 'node:assert/strict';
import test from 'node:test';

import { CONTROL, FLAG_END, KIND, Reader, encode } from '../src/wire.js';

test('a frame survives the codec unchanged', () => {
  const frame = {
    sequence: 3,
    patches: [
      { Create: { id: 1, tag: 'Card' } },
      { Insert: { parent: 0, child: 1, before: null } },
      { SetProp: { id: 1, prop: 'Label', value: { Text: 'Go' } } },
    ],
  };
  const payload = { call: 'interface_render', with: { frame } };
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

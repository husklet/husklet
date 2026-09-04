import assert from 'node:assert/strict';
import test from 'node:test';

import { HEX_VIEW_BYTE_LIMIT, formatHex } from '../dist/binary-inspection.js';

test('binary projection has 16-byte rows and printable substitution', () => {
  const rendered = formatHex(Uint8Array.from([0x41, 0, 0x7a]));
  assert.equal(rendered, '00000000  41 00 7a                                          |A.z|');
});

test('binary projection has a hard visible ceiling', () => {
  const rendered = formatHex(new Uint8Array(HEX_VIEW_BYTE_LIMIT + 32), 9000);
  assert.equal(rendered.split('\n').length, 257);
  assert.match(rendered, /truncated: showing 4096 of 9000 bytes/);
});

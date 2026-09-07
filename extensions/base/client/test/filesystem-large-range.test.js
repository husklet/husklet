import assert from 'node:assert/strict';
import test from 'node:test';

import { workspace } from '../dist/index.js';

test('large filesystem offsets remain paged while unsafe ranges fail before framing', async () => {
  const calls = [];
  const session = {
    granted: [],
    onEvent() {
      return () => false;
    },
    async call(name, argument) {
      calls.push({ name, argument });
      return {
        reply: 'file_range',
        with: {
          path: argument.path,
          identity: 'large-v1',
          offset: argument.offset,
          total: argument.offset + 2,
          contents: [111, 107],
          eof: true,
          truncated: false,
        },
      };
    },
  };
  const files = workspace(session).files;

  for (const [offset, limit] of [
    [-1, 1],
    [Number.MAX_SAFE_INTEGER + 1, 1],
    [0, 0],
    [0, 65_537],
  ]) {
    await assert.rejects(files.readRange('data/embeddings.bin', offset, limit), /filesystem range/);
  }
  assert.equal(calls.length, 0, 'invalid ranges fail before the transport');

  const offset = 8 * 1024 * 1024;
  const page = await files.readRange('data/embeddings.bin', offset, 2, 'large-v1');
  assert.deepEqual(calls, [
    {
      name: 'filesystem_read_range',
      argument: {
        path: 'data/embeddings.bin',
        offset,
        limit: 2,
        observed: 'large-v1',
      },
    },
  ]);
  assert.equal(page.offset, offset);
  assert.deepEqual(page.contents, [111, 107]);
});

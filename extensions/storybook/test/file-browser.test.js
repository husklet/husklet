import assert from 'node:assert/strict';
import test from 'node:test';
import { FILE_SOURCE, FileSource } from '../dist/file-browser.js';

test('file browsing materializes only the requested bounded window', () => {
  const source = new FileSource();
  const window = source.answer({ source: FILE_SOURCE, version: 1, id: 7, range: { start: 20, count: 100 } });
  assert.equal(window.rows.length, 32);
  assert.equal(window.rows[0].cells[0].Text, 'src/module-20.rs');
  assert.equal(source.answer({ source: 999, version: 1, id: 8, range: { start: 0, count: 1 } }), null);
  assert.equal(source.answer(null), null);
  assert.equal(source.answer({ source: FILE_SOURCE, version: 1, id: 9, range: { start: -1, count: 1 } }), null);
});

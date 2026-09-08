import assert from 'node:assert/strict';
import { readFile, readdir } from 'node:fs/promises';
import test from 'node:test';

test('Top product surfaces never expose the generic ObjectInspector', async () => {
  const root = new URL('../src/', import.meta.url);
  const files = (await readdir(root, { recursive: true }))
    .filter((name) => /\.(?:ts|tsx)$/.test(name))
    .sort();
  const offenders = [];
  for (const name of files) {
    const source = await readFile(new URL(name, root), 'utf8');
    if (source.includes('ObjectInspector')) offenders.push(name);
  }
  assert.deepEqual(offenders, [], 'known host models require domain-specific product views');
});

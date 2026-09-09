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

test('workspace Switch captions use one native FormControlLabel click target', async () => {
  const source = await readFile(new URL('../src/workspace.tsx', import.meta.url), 'utf8');
  for (const label of ['Cursor blink', 'Read only']) {
    assert.match(
      source,
      new RegExp(`<FormControlLabel label="${label}"[\\s\\S]*?<Switch`),
      `${label} must label its sole Switch`,
    );
  }
  assert.doesNotMatch(source, /<Switch[\s\S]{0,160}\/>\s*<Text label="(?:Cursor blink|Read only)"/);
});

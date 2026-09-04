import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import test from 'node:test';
import { SECTIONS } from '../dist/app.js';

test('the pane chooser advertises every compact manager view exactly once', async () => {
  const manifest = await readFile(new URL('../extension.toml', import.meta.url), 'utf8');
  const providers = [...manifest.matchAll(/^id = "([^"]+)"$/gm)].map((match) => match[1]);

  assert.deepEqual(providers, SECTIONS);
  assert.equal(new Set(providers).size, providers.length);
});

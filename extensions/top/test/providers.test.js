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

test('Top requests the complete workspace management and pane authority it exercises', async () => {
  const manifest = await readFile(new URL('../extension.toml', import.meta.url), 'utf8');
  const capabilities = new Set(manifest.match(/^capabilities = \[(.*)\]$/m)?.[1]
    .split(',').map((value) => value.trim().replaceAll('"', '')) ?? []);

  for (const capability of ['workspaces:read', 'workspaces:control', 'extensions:read', 'extensions:control', 'extensions:install', 'terminals:read', 'terminals:control', 'terminals:output', 'panes:observe', 'panes:semantic-read', 'panes:semantic-control']) {
    assert.ok(capabilities.has(capability), capability);
  }
});

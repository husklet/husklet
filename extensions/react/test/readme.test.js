import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');

test('every documented JavaScript example is valid in the transform-free Node starter', () => {
  const readme = fs.readFileSync(path.join(root, 'README.md'), 'utf8');
  const blocks = [...readme.matchAll(/^```js\n([\s\S]*?)^```$/gm)].map((match) => match[1]);
  assert(blocks.length >= 3, 'expected primary, pane-provider, and workspace API examples');
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'husklet-readme-'));
  try {
    blocks.forEach((source, index) => {
      const file = path.join(directory, `example-${index}.mjs`);
      fs.writeFileSync(file, source);
      execFileSync(process.execPath, ['--check', file], { stdio: 'pipe' });
    });
  } finally {
    fs.rmSync(directory, { recursive: true, force: true });
  }
  assert(!readme.includes('```jsx'), 'starter docs must not imply an unavailable JSX transform');
});

test('workspace update documentation preserves inspected immutable authority', () => {
  const readme = fs.readFileSync(path.join(root, 'README.md'), 'utf8');
  assert.match(readme, /if \(!configuration\.generation\) throw new Error/);
  assert.match(readme, /host\.update\('backend', configuration\.generation, \{ \.\.\.configuration, memory_mb: 4096 \}\)/);
});

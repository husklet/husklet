import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';

import { determinePublication } from '../../tools/publication-plan.js';

const local = { client: 'sha512-client', react: 'sha512-react' };

test('a new version publishes the dependency before React', () => {
  assert.deepEqual(determinePublication(local, { client: null, react: null }), { client: true, react: true });
});

test('a retry resumes after the exact client tarball was already published', () => {
  assert.deepEqual(determinePublication(local, { client: local.client, react: null }), { client: false, react: true });
  assert.deepEqual(determinePublication(local, local), { client: false, react: false });
});

test('a duplicate version fails closed on different contents or impossible order', () => {
  assert.throws(
    () => determinePublication(local, { client: 'sha512-other', react: null }),
    /different tarball integrity/,
  );
  assert.throws(
    () => determinePublication(local, { client: null, react: local.react }),
    /dependency is absent/,
  );
});

test('the release command emits a resumable GitHub Actions publication plan', () => {
  const scratch = fs.mkdtempSync(path.join(os.tmpdir(), 'husklet-publication-plan-'));
  try {
    const npm = path.join(scratch, 'npm');
    const output = path.join(scratch, 'output');
    fs.writeFileSync(npm, `#!/usr/bin/env node
      const args = process.argv.slice(2);
      if (args[0] === 'pack') {
        const name = args.at(-1).split('/').at(-1);
        process.stdout.write(JSON.stringify([{ integrity: 'sha512-' + name }]));
      } else if (args[1].startsWith('@husklet/client@')) {
        process.stdout.write(JSON.stringify('sha512-client'));
      } else {
        process.stderr.write('npm error code E404\\n404 Not Found');
        process.exit(1);
      }
    `, { mode: 0o755 });
    const extensions = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
    execFileSync(process.execPath, ['tools/publication-plan.js'], {
      cwd: extensions,
      env: {
        ...process.env,
        PATH: `${scratch}:${process.env.PATH}`,
        RELEASE_VERSION: '1.2.3',
        GITHUB_OUTPUT: output,
      },
      stdio: 'pipe',
    });
    assert.equal(fs.readFileSync(output, 'utf8'), 'client=false\nreact=true\n');
  } finally {
    fs.rmSync(scratch, { recursive: true, force: true });
  }
});

import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../../../..');
const packageRoot = (name) => path.join(root, 'extensions', 'base', name);
const read = (name) => JSON.parse(fs.readFileSync(path.join(packageRoot(name), 'package.json')));
const client = read('client');
const clientStarter = JSON.parse(
  fs.readFileSync(path.join(root, 'extensions/base/client/examples/starter/package.json')),
);
const react = read('react');
const starter = JSON.parse(
  fs.readFileSync(path.join(root, 'extensions/base/react/examples/starter/package.json')),
);
const expected = process.env.RELEASE_VERSION ?? client.version;
for (const manifest of [client, react]) {
  assert.equal(manifest.version, expected);
  assert.deepEqual(manifest.publishConfig, {
    access: 'public',
    provenance: true,
  });
}
assert.equal(react.dependencies['@husklet/client'], expected);
assert.equal(clientStarter.dependencies['@husklet/client'], expected);
assert.equal(starter.dependencies['@husklet/client'], expected);
assert.equal(starter.dependencies['@husklet/react'], expected);

const directoryContents = (directory) => {
  const files = new Map();
  const visit = (current, prefix = '') => {
    for (const entry of fs.readdirSync(current, { withFileTypes: true })) {
      const relative = path.join(prefix, entry.name);
      const absolute = path.join(current, entry.name);
      if (entry.isDirectory()) visit(absolute, relative);
      else if (entry.isFile()) files.set(relative, fs.readFileSync(absolute));
    }
  };
  visit(directory);
  return files;
};

// The client dist is committed because the base image packs with lifecycle
// scripts disabled. Rebuilding must reproduce it exactly rather than silently
// repairing a stale release artifact. React dist is intentionally untracked,
// so build it before the same script-disabled pack.
const clientDist = path.join(packageRoot('client'), 'dist');
assert(fs.existsSync(clientDist), 'the committed client dist is missing');
const expectedClientDist = directoryContents(clientDist);
execFileSync('npm', ['run', 'build', '--prefix', packageRoot('client')], { stdio: 'pipe' });
assert.deepEqual(
  directoryContents(clientDist),
  expectedClientDist,
  'the committed client dist is stale; run npm build in extensions/base/client',
);
execFileSync('npm', ['run', 'build', '--prefix', packageRoot('react')], { stdio: 'pipe' });

const scratch = fs.mkdtempSync(path.join(os.tmpdir(), 'husklet-pack-'));
try {
  const pack = (name) => {
    const result = JSON.parse(
      execFileSync('npm', ['pack', '--json', '--ignore-scripts', '--pack-destination', scratch], {
        cwd: packageRoot(name),
        encoding: 'utf8',
      }),
    )[0];
    assert.equal(result.name, `@husklet/${name}`);
    assert(result.integrity.startsWith('sha512-'));
    return path.join(scratch, result.filename);
  };
  const clientTarball = pack('client');
  const reactTarball = pack('react');
  const consumer = path.join(scratch, 'consumer');
  fs.mkdirSync(consumer);
  fs.writeFileSync(
    path.join(consumer, 'package.json'),
    JSON.stringify({ private: true, type: 'module' }),
  );
  execFileSync(
    'npm',
    [
      'install',
      '--ignore-scripts',
      '--no-audit',
      '--no-fund',
      clientTarball,
      reactTarball,
      'react@18.3.1',
    ],
    { cwd: consumer },
  );
  execFileSync(
    process.execPath,
    [
      '--input-type=module',
      '--eval',
      "import { Session, workspace } from '@husklet/client'; import { Session as ReactSession } from '@husklet/react'; if (Session !== ReactSession || typeof workspace !== 'function') process.exit(1)",
    ],
    { cwd: consumer },
  );
  fs.writeFileSync(
    path.join(consumer, 'consumer.ts'),
    `
import { Session, workspace } from '@husklet/client';
import type { ExtensionCapability, PaneText as WirePaneText } from '@husklet/client/protocol';
import { Button, type ButtonProps } from '@husklet/react';
import type { ComponentType } from 'react';

declare const session: Session;
const host = workspace(session);
const panes = host.terminal.panes();
const button: ComponentType<ButtonProps> = Button;
const capability: ExtensionCapability = 'panes:observe';
// @ts-expect-error generated capabilities are closed and namespaced.
const invalidCapability: ExtensionCapability = 'container-read';
const projection: WirePaneText = { slot: 'pane-1', lines: ['ready'], truncated: false };
void panes;
void button;
void capability;
void invalidCapability;
void projection;
`,
  );
  fs.writeFileSync(
    path.join(consumer, 'tsconfig.json'),
    JSON.stringify({
      compilerOptions: {
        strict: true,
        noEmit: true,
        target: 'ES2022',
        module: 'NodeNext',
        moduleResolution: 'NodeNext',
        skipLibCheck: false,
      },
      include: ['consumer.ts'],
    }),
  );
  execFileSync(
    path.join(root, 'extensions/node_modules/.bin/tsc'),
    ['--project', 'tsconfig.json'],
    {
      cwd: consumer,
    },
  );
} finally {
  fs.rmSync(scratch, { recursive: true, force: true });
}

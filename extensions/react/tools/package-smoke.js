import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const scratch = fs.mkdtempSync(path.join(os.tmpdir(), 'husklet-react-pack-'));

try {
  const dryRun = JSON.parse(execFileSync('npm', ['pack', '--dry-run', '--json', '--ignore-scripts'], {
    cwd: root, encoding: 'utf8',
  }));
  const names = new Set(dryRun[0].files.map(({ path: name }) => name));
  for (const required of ['package.json', 'README.md', 'LICENSE', 'catalogue.json', 'src/index.js', 'src/index.d.ts']) {
    assert(names.has(required), `npm package omits ${required}`);
  }
  assert(![...names].some((name) => name.startsWith('test/') || name.startsWith('tools/')), 'developer-only files leaked into package');

  const tarball = execFileSync('npm', ['pack', '--json', '--ignore-scripts', '--pack-destination', scratch], {
    cwd: root, encoding: 'utf8',
  });
  const filename = JSON.parse(tarball)[0].filename;
  const consumer = path.join(scratch, 'consumer');
  fs.mkdirSync(consumer);
  fs.writeFileSync(path.join(consumer, 'package.json'), JSON.stringify({ private: true, type: 'module' }));
  execFileSync('npm', ['install', '--ignore-scripts', '--no-audit', '--no-fund', path.join(scratch, filename), 'react@18.3.1'], {
    cwd: consumer, stdio: 'pipe',
  });
  const runtime = execFileSync(process.execPath, ['--input-type=module', '--eval', `
    import { Button, acceptsChildren, connect, tags, workspace } from '@husklet/react';
    import catalogue from '@husklet/react/catalogue' with { type: 'json' };
    if (typeof connect !== 'function' || typeof workspace !== 'function') process.exit(1);
    if (Button !== 'Button' || !acceptsChildren('Column')) process.exit(2);
    if (catalogue.tags.length !== tags.length || catalogue.tags[0].name !== tags[0]) process.exit(3);
  `], { cwd: consumer, encoding: 'utf8' });
  assert.equal(runtime, '');
  const manifest = JSON.parse(fs.readFileSync(path.join(consumer, 'node_modules/@husklet/react/package.json'), 'utf8'));
  assert.equal(manifest.exports['.'].types, './src/index.d.ts');

  fs.writeFileSync(path.join(consumer, 'consumer.ts'), `
    import { render, useHostEvents, usePaneSelection, workspace, type HostEvent, type InterfaceEvent, type InterfaceSourceMutation, type Session, type ProcessList } from '@husklet/react';
    declare const session: Session;
    const api = workspace(session);
    const table: Promise<ProcessList> = api.containers.processes('container');
    void table;
    void api.containers.exec('container', { command: ['sh'], workingDirectory: '/work' });
    void api.subscribe('terminal');
    void api.subscribe('volumes');
    void api.subscribe('extensions');
    void api.watchExtensions((extensions) => { void extensions[0]?.name; });
    void api.watchExtensionAcquisitions((change) => { void change.revision; });
    const surface = render(null, session, { split: { slot: 'pane-1', division: 'beside' } });
    const mutation: InterfaceSourceMutation = { Length: { source: 1, version: 2, rows: 100_000 } };
    void surface.ready;
    void surface.source(mutation);
    const event: HostEvent = { pane_provider: 'logs', slot: 'pane-1' };
    if ('pane_provider' in event) {
      const provider: string = event.pane_provider;
      const slot: string = event.slot;
      void provider; void slot;
    }
    const interaction: HostEvent = { interaction: 'focus', trigger: 'Focus', node: 1, id: '1:Focus', focused: true };
    void interaction;
    function describeInterfaceEvent(received: InterfaceEvent): string {
      switch (received.interaction) {
        case 'key': {
          const key: string = received.key;
          // @ts-expect-error a key event does not carry pointer coordinates
          void received.x;
          return key;
        }
        case 'pointer': {
          const phase: 'enter' | 'motion' | 'leave' | 'press' | 'release' = received.phase;
          const x: number | null = received.x;
          return phase + ':' + x;
        }
        case 'select': return received.rows.join(',');
        case 'scroll': return received.dx + ',' + received.dy;
        case 'context': return received.x + ',' + received.y;
        case 'focus': return String(received.focused);
        case 'change': return String(received.value);
        case 'invoke': case 'submit': case 'close': return received.id;
        default: { const exhaustive: never = received; return exhaustive; }
      }
    }
    void describeInterfaceEvent;
    function ProviderView() {
      useHostEvents(session, (received, channel) => { void received; void channel; });
      const selected = usePaneSelection(session, 'logs');
      return selected?.slot ?? null;
    }
    void ProviderView;
  `);
  execFileSync(path.resolve(root, '../node_modules/.bin/tsc'), [
    '--noEmit', '--strict', '--skipLibCheck', '--target', 'ES2022',
    '--module', 'NodeNext', '--moduleResolution', 'NodeNext', 'consumer.ts',
  ], { cwd: consumer, stdio: 'pipe' });

  const dockerfile = fs.readFileSync(path.join(root, 'Dockerfile'), 'utf8');
  const readme = fs.readFileSync(path.join(root, 'README.md'), 'utf8');
  assert.match(dockerfile, /^ARG NODE_IMAGE=node:22-alpine$/m);
  assert.match(dockerfile, /FROM \$\{NODE_IMAGE\} AS package/);
  assert.match(dockerfile, /npm pack --ignore-scripts/);
  assert.match(dockerfile, /^USER node$/m);
  assert.match(dockerfile, /HUSKLET_EXTENSION_SOCKET=\/run\/husklet\/extension\.sock/);
  assert(!dockerfile.includes('--platform='), 'base image must not pin one architecture');
  assert.match(readme, /npm install @husklet\/react react@18\.3\.1/);
  assert.match(readme, /ghcr\.io\/husklet\/husklet\/extension-react-base:latest/);
} finally {
  fs.rmSync(scratch, { recursive: true, force: true });
}

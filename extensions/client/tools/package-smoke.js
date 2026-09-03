import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const scratch = fs.mkdtempSync(path.join(os.tmpdir(), 'husklet-client-pack-'));

try {
  const packed = JSON.parse(execFileSync('npm', [
    'pack', '--json', '--ignore-scripts', '--pack-destination', scratch,
  ], { cwd: root, encoding: 'utf8' }))[0];
  const names = new Set(packed.files.map(({ path: name }) => name));
  for (const required of [
    'package.json', 'README.md', 'LICENSE', 'src/index.js', 'src/index.d.ts',
    'src/generated-protocol.js', 'src/generated-protocol.d.ts', 'src/semantic.js',
    'src/session.js', 'src/wire.js',
  ]) assert(names.has(required), `npm package omits ${required}`);
  assert(![...names].some((name) => name.startsWith('test/') || name.startsWith('tools/')),
    'developer-only files leaked into package');

  const consumer = path.join(scratch, 'consumer');
  fs.mkdirSync(consumer);
  fs.writeFileSync(path.join(consumer, 'package.json'), JSON.stringify({ private: true, type: 'module' }));
  execFileSync('npm', ['install', '--ignore-scripts', '--no-audit', '--no-fund', path.join(scratch, packed.filename)], {
    cwd: consumer, stdio: 'pipe',
  });
  const manifest = JSON.parse(fs.readFileSync(path.join(consumer, 'node_modules/@husklet/client/package.json')));
  assert.equal(manifest.dependencies, undefined, 'framework-neutral client gained an undeclared runtime dependency');
  assert.equal(manifest.exports['.'].types, './src/index.d.ts');
  assert.equal(manifest.exports['./protocol'].types, './src/generated-protocol.d.ts');

  execFileSync(process.execPath, ['--input-type=module', '--eval', `
    import { PROTOCOL_VERSION, Session, semanticXml, workspace } from '@husklet/client';
    import { PROTOCOL_VERSION as subpathVersion, validateRequest } from '@husklet/client/protocol';
    if (PROTOCOL_VERSION !== subpathVersion || typeof Session !== 'function'
      || typeof workspace !== 'function' || typeof semanticXml !== 'function'
      || typeof validateRequest !== 'function') process.exit(1);
  `], { cwd: consumer, stdio: 'pipe' });

  fs.writeFileSync(path.join(consumer, 'consumer.ts'), `
    import {
      semanticXml, workspace, type ContainerCreateSpec, type PaneSemanticAction,
      type PaneSemanticTree, type ProcessList, type Session, type TerminalTopology,
      type WorkspaceConfiguration,
    } from '@husklet/client';
    import { PROTOCOL_VERSION, type WireRequest } from '@husklet/client/protocol';
    declare const session: Session;
    const host = workspace(session);
    const configuration: Promise<WorkspaceConfiguration> = host.inspect('project');
    const create: ContainerCreateSpec = { image: 'alpine:3.20', name: 'worker', command: ['sleep', '10'] };
    const container: Promise<string> = host.containers.create(create);
    const processes: Promise<ProcessList> = host.containers.processes('a'.repeat(64));
    const topology: Promise<TerminalTopology> = host.terminal.topology();
    const text = host.terminal.read('pane-1', 200);
    const input = host.terminal.writeInput('pane-1', 4, 9, new Uint8Array([3]));
    const action: PaneSemanticAction = { generation: 4, revision: 9, node: 2, action: 'invoke' };
    const acted = host.terminal.act('pane-1', action);
    declare const tree: PaneSemanticTree;
    const xml: string = semanticXml(tree);
    const request: WireRequest = { call: 'workspace_info' };
    const protocol: 1 = PROTOCOL_VERSION;
    void configuration; void container; void processes; void topology; void text; void input;
    void acted; void xml; void request; void protocol;
  `);
  execFileSync(path.resolve(root, '../node_modules/.bin/tsc'), [
    '--noEmit', '--strict', '--skipLibCheck', '--target', 'ES2022',
    '--module', 'NodeNext', '--moduleResolution', 'NodeNext', 'consumer.ts',
  ], { cwd: consumer, stdio: 'pipe' });
} finally {
  fs.rmSync(scratch, { recursive: true, force: true });
}

import assert from 'node:assert/strict';
import { execFileSync, spawn } from 'node:child_process';
import fs from 'node:fs';
import net from 'node:net';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const scratch = fs.mkdtempSync(path.join(os.tmpdir(), 'husklet-client-pack-'));

async function runPackedStarter(starter, installedClient) {
  const wire = await import(new URL('src/wire.js', `file://${installedClient}/`));
  const socket = path.join(starter, 'host.sock');
  let peer;
  let observed = false;
  const server = net.createServer((stream) => {
    peer = stream;
    const reader = new wire.Reader();
    stream.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.channel !== 2 || frame.kind !== wire.KIND.request) continue;
        assert.deepEqual(frame.payload, { call: 'workspace_info' });
        observed = true;
        stream.write(wire.encode({
          channel: 2,
          kind: wire.KIND.response,
          payload: { reply: 'workspace', with: { name: 'project', architecture: 'x86_64', image: 'alpine:3.20' } },
        }));
      }
    });
    stream.write(wire.encode({
      channel: 0,
      kind: wire.KIND.open,
      payload: { protocol: 1, extension: 'client-starter', granted: ['workspace-read'] },
    }));
  });
  await new Promise((resolve, reject) => { server.once('error', reject); server.listen(socket, resolve); });
  const child = spawn(process.execPath, ['main.js'], {
    cwd: starter,
    env: { ...process.env, HUSKLET_EXTENSION_SOCKET: socket },
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  let stdout = ''; let stderr = '';
  child.stdout.setEncoding('utf8'); child.stdout.on('data', (chunk) => { stdout += chunk; });
  child.stderr.setEncoding('utf8'); child.stderr.on('data', (chunk) => { stderr += chunk; });
  try {
    for (let attempt = 0; attempt < 400 && !stdout.endsWith('\n'); attempt += 1) {
      if (child.exitCode !== null) break;
      await new Promise((resolve) => setTimeout(resolve, 5));
    }
    assert(observed, `packed client starter did not call the real socket; stderr=${stderr}`);
    assert(stdout.endsWith('\n'), `packed client starter did not report workspace information; stderr=${stderr}`);
    child.kill('SIGTERM');
    const exit = await Promise.race([
      new Promise((resolve) => child.once('exit', (code, signal) => resolve({ code, signal }))),
      new Promise((_, reject) => setTimeout(() => reject(new Error('packed client starter did not stop')), 2_000)),
    ]);
    assert.deepEqual(exit, { code: 0, signal: null });
    assert.equal(stdout, '{"name":"project","architecture":"x86_64","image":"alpine:3.20"}\n');
    assert.equal(stderr, '');
  } finally {
    peer?.destroy();
    if (child.exitCode === null) child.kill('SIGKILL');
    await new Promise((resolve) => server.close(resolve));
  }
}

try {
  const packed = JSON.parse(execFileSync('npm', [
    'pack', '--json', '--ignore-scripts', '--pack-destination', scratch,
  ], { cwd: root, encoding: 'utf8' }))[0];
  const names = new Set(packed.files.map(({ path: name }) => name));
  for (const required of [
    'package.json', 'README.md', 'API.md', 'LICENSE', 'src/index.js', 'src/index.d.ts',
    'src/generated-protocol.js', 'src/generated-protocol.d.ts', 'src/semantic.js',
    'src/session.js', 'src/wire.js', 'examples/starter/Dockerfile',
    'examples/starter/extension.toml', 'examples/starter/main.js', 'examples/starter/package.json',
  ]) assert(names.has(required), `npm package omits ${required}`);
  assert(![...names].some((name) => name.startsWith('test/') || name.startsWith('tools/')),
    'developer-only files leaked into package');

  const consumer = path.join(scratch, 'consumer');
  fs.mkdirSync(consumer);
  fs.writeFileSync(path.join(consumer, 'package.json'), JSON.stringify({ private: true, type: 'module' }));
  execFileSync('npm', ['install', '--ignore-scripts', '--no-save', '--offline', '--no-audit', '--no-fund', path.join(scratch, packed.filename)], {
    cwd: consumer, stdio: 'pipe',
  });
  const manifest = JSON.parse(fs.readFileSync(path.join(consumer, 'node_modules/@husklet/client/package.json')));
  assert.equal(manifest.dependencies, undefined, 'framework-neutral client gained an undeclared runtime dependency');
  for (const field of ['cpu', 'os', 'libc']) {
    assert.equal(manifest[field], undefined, `framework-neutral client gained an architecture restriction in ${field}`);
  }
  assert.equal(manifest.exports['.'].types, './src/index.d.ts');
  assert.equal(manifest.exports['./protocol'].types, './src/generated-protocol.d.ts');

  const installedClient = path.join(consumer, 'node_modules/@husklet/client');
  const apiReference = fs.readFileSync(path.join(installedClient, 'API.md'), 'utf8');
  const examples = [...apiReference.matchAll(/```js\n([\s\S]*?)```/g)].map((match) => match[1]);
  assert(examples.length >= 2, 'packed API reference must carry short JavaScript examples');
  for (const [index, example] of examples.entries()) {
    const source = path.join(consumer, `api-example-${index}.mjs`);
    fs.writeFileSync(source, example);
    execFileSync(process.execPath, ['--check', source], { stdio: 'pipe' });
  }
  const starter = path.join(scratch, 'starter');
  fs.cpSync(path.join(installedClient, 'examples/starter'), starter, { recursive: true });
  const starterManifest = JSON.parse(fs.readFileSync(path.join(starter, 'package.json')));
  assert.equal(starterManifest.dependencies['@husklet/client'], manifest.version);
  execFileSync('npm', ['install', '--ignore-scripts', '--no-save', '--offline', '--no-audit', '--no-fund', path.join(scratch, packed.filename)], {
    cwd: starter, stdio: 'pipe',
  });
  execFileSync('npm', ['test'], { cwd: starter, stdio: 'pipe' });
  const dockerfile = fs.readFileSync(path.join(starter, 'Dockerfile'), 'utf8');
  assert.match(dockerfile, /^ARG NODE_IMAGE=node:22-alpine@sha256:[0-9a-f]{64}$/m);
  assert.match(dockerfile, /COPY --chown=node:node package\.json/);
  assert.match(dockerfile, /COPY --chown=node:node node_modules\/@husklet\/client \.\/node_modules\/@husklet\/client/);
  assert.match(dockerfile, /require\('@husklet\/client\/package\.json'\)\.version/);
  assert.doesNotMatch(dockerfile, /^RUN npm (?:ci|install)/m, 'image build must not resolve the npm registry');
  assert.match(dockerfile, /^USER node$/m);
  assert.match(dockerfile, /LABEL husklet\.extension\.manifest="\/etc\/husklet\/extension\.toml"/);
  await runPackedStarter(starter, path.join(starter, 'node_modules/@husklet/client'));

  execFileSync(process.execPath, ['--input-type=module', '--eval', `
    import { PROTOCOL_VERSION, Session, protocolSurface, semanticXml, workspace } from '@husklet/client';
    import { PROTOCOL_VERSION as subpathVersion, validateRequest, validateUiEvent } from '@husklet/client/protocol';
    const event = validateUiEvent({ interaction: 'drop', trigger: 'Drop', node: 1, id: 'drop-1', source: 2, x: 3, y: 4 });
    if (PROTOCOL_VERSION !== subpathVersion || typeof Session !== 'function'
      || typeof workspace !== 'function' || typeof semanticXml !== 'function'
      || protocolSurface.requests.workspace_info.api !== 'info'
      || typeof validateRequest !== 'function' || event.interaction !== 'drop') process.exit(1);
    try {
      validateUiEvent({ interaction: 'drop', trigger: 'Drop', node: 1, id: 'drop-1', x: 3, y: 4 });
      process.exit(1);
    } catch (error) {
      if (!(error instanceof TypeError)) process.exit(1);
    }
  `], { cwd: consumer, stdio: 'pipe' });

  fs.writeFileSync(path.join(consumer, 'consumer.ts'), `
    import {
      semanticXml, workspace, type ConnectOptions, type ContainerCreateSpec, type PaneSemanticAction,
      type PaneSemanticTree, type ProcessList, type Session, type TerminalTopology,
      type WorkspaceConfiguration,
    } from '@husklet/client';
    import { PROTOCOL_VERSION, type WireRequest, type WireUiEvent } from '@husklet/client/protocol';
    declare const session: Session;
    const host = workspace(session);
    const cancellable = host.withSignal(new AbortController().signal);
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
    const event: WireUiEvent = { interaction: 'key', trigger: 'Key', node: 1, id: 'key-1', key: 'Enter', keycode: 13, modifiers: 0, pressed: true };
    const protocol: 1 = PROTOCOL_VERSION;
    const lifecycle: ConnectOptions = { connectTimeout: 5_000, onClose: (error) => { void error.message; } };
    void configuration; void container; void processes; void topology; void text; void input;
    void acted; void xml; void request; void event; void protocol; void lifecycle; void cancellable.info();
  `);
  execFileSync(path.resolve(root, '../node_modules/.bin/tsc'), [
    '--noEmit', '--strict', '--skipLibCheck', '--target', 'ES2022',
    '--module', 'NodeNext', '--moduleResolution', 'NodeNext', 'consumer.ts',
  ], { cwd: consumer, stdio: 'pipe' });
} finally {
  fs.rmSync(scratch, { recursive: true, force: true });
}

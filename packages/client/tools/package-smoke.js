import assert from 'node:assert/strict';
import { execFileSync, spawn, spawnSync } from 'node:child_process';
import fs from 'node:fs';
import net from 'node:net';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const scratch = fs.mkdtempSync(path.join(os.tmpdir(), 'husklet-client-pack-'));

async function runPackedStarter(starter, installedClient, architecture, signal, hostEof = false) {
  const wire = await import(new URL('src/wire.js', `file://${installedClient}/`));
  const socket = path.join(starter, `host-${architecture}-${signal}${hostEof ? '-eof' : ''}.sock`);
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
          payload: { reply: 'workspace', with: { name: 'project', architecture, image: 'alpine:3.20' } },
        }), () => { if (hostEof) stream.end(); });
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
    if (!hostEof) child.kill(signal);
    const exit = child.exitCode !== null ? { code: child.exitCode, signal: child.signalCode } : await Promise.race([
      new Promise((resolve) => child.once('exit', (code, receivedSignal) => resolve({ code, signal: receivedSignal }))),
      new Promise((_, reject) => setTimeout(() => reject(new Error('packed client starter did not stop')), 2_000)),
    ]);
    assert.deepEqual(exit, hostEof ? { code: 1, signal: null } : { code: 0, signal: null });
    assert.equal(stdout, `${JSON.stringify({ name: 'project', architecture, image: 'alpine:3.20' })}\n`);
    assert.equal(stderr, hostEof
      ? 'client-starter: host connection ended: extension host connection closed\n'
      : '');
  } finally {
    peer?.destroy();
    if (child.exitCode === null) child.kill('SIGKILL');
    await new Promise((resolve) => server.close(resolve));
  }
}

async function runPackedProtocolRefusal(starter, installedClient) {
  const wire = await import(new URL('src/wire.js', `file://${installedClient}/`));
  const socket = path.join(starter, 'host-protocol-refusal.sock');
  let spoke = false;
  let peer;
  const server = net.createServer((stream) => {
    peer = stream;
    const reader = new wire.Reader();
    stream.on('data', (chunk) => {
      spoke = true;
      for (const frame of reader.take(chunk)) {
        if (frame.payload?.call === 'workspace_info') stream.write(wire.encode({
          channel: frame.channel,
          kind: wire.KIND.response,
          payload: { reply: 'workspace', with: { name: 'project', architecture: 'x86_64', image: 'alpine:3.20' } },
        }));
      }
    });
    stream.write(wire.encode({
      channel: 0,
      kind: wire.KIND.open,
      payload: { protocol: 2, extension: 'client-starter', granted: ['workspace-read'] },
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
    const exit = await Promise.race([
      new Promise((resolve) => child.once('exit', (code, signal) => resolve({ code, signal }))),
      new Promise((_, reject) => setTimeout(() => reject(new Error('packed client starter did not refuse incompatible protocol')), 2_000)),
    ]);
    assert.deepEqual(exit, { code: 1, signal: null });
    assert.equal(spoke, false, 'incompatible protocol must be refused before the starter sends a request');
    assert.equal(stdout, '');
    assert.equal(stderr, 'client-starter: startup failed: host speaks protocol 2, this extension speaks 1\n');
  } finally {
    peer?.destroy();
    if (child.exitCode === null) child.kill('SIGKILL');
    await new Promise((resolve) => server.close(resolve));
  }
}

async function runPackedTruncatedGreeting(starter, installedClient) {
  const wire = await import(new URL('src/wire.js', `file://${installedClient}/`));
  const socket = path.join(starter, 'host-truncated-greeting.sock');
  let spoke = false;
  let peer;
  const server = net.createServer((stream) => {
    peer = stream;
    stream.on('data', () => { spoke = true; });
    const greeting = wire.encode({
      channel: 0,
      kind: wire.KIND.open,
      payload: { protocol: 1, extension: 'client-starter', granted: ['workspace-read'] },
    });
    // A clean EOF in the middle of a frame must not be mistaken for a host
    // that never greeted us or leave a day-one starter waiting indefinitely.
    stream.end(greeting.subarray(0, greeting.length - 1));
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
    const exit = await Promise.race([
      new Promise((resolve) => child.once('exit', (code, signal) => resolve({ code, signal }))),
      new Promise((_, reject) => setTimeout(() => reject(new Error('packed client starter did not reject a truncated greeting')), 2_000)),
    ]);
    assert.deepEqual(exit, { code: 1, signal: null });
    assert.equal(spoke, false, 'truncated greeting must be rejected before the starter sends a request');
    assert.equal(stdout, '');
    assert.match(stderr, /^client-starter: startup failed: extension host closed with an unfinished frame \([1-9][0-9]* bytes buffered\)\n$/);
  } finally {
    peer?.destroy();
    if (child.exitCode === null) child.kill('SIGKILL');
    await new Promise((resolve) => server.close(resolve));
  }
}

async function runPackedOversizedGreeting(starter, installedClient) {
  const wire = await import(new URL('src/wire.js', `file://${installedClient}/`));
  const socket = path.join(starter, 'host-oversized-greeting.sock');
  let spoke = false;
  let peer;
  const server = net.createServer((stream) => {
    peer = stream;
    stream.on('data', () => { spoke = true; });
    const header = Buffer.alloc(wire.HEADER);
    header.writeUInt32LE(wire.PAYLOAD_LIMIT + 1, 0);
    header.writeUInt32LE(0, 4);
    header.writeUInt8(wire.KIND.open, 8);
    header.writeUInt8(wire.FLAG_END, 9);
    stream.end(header);
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
    const exit = await Promise.race([
      new Promise((resolve) => child.once('exit', (code, signal) => resolve({ code, signal }))),
      new Promise((_, reject) => setTimeout(() => reject(new Error('packed client starter did not reject an oversized greeting')), 2_000)),
    ]);
    assert.deepEqual(exit, { code: 1, signal: null });
    assert.equal(spoke, false, 'oversized greeting must be rejected before the starter sends a request');
    assert.equal(stdout, '');
    assert.equal(stderr, `client-starter: startup failed: frame declares ${wire.PAYLOAD_LIMIT + 1} bytes, above the ${wire.PAYLOAD_LIMIT} limit\n`);
  } finally {
    peer?.destroy();
    if (child.exitCode === null) child.kill('SIGKILL');
    await new Promise((resolve) => server.close(resolve));
  }
}

async function runPackedIllegalHeader(starter, installedClient) {
  const wire = await import(new URL('src/wire.js', `file://${installedClient}/`));
  const socket = path.join(starter, 'host-illegal-header.sock');
  let spoke = false;
  let peer;
  const server = net.createServer((stream) => {
    peer = stream;
    stream.on('data', () => { spoke = true; });
    const greeting = wire.encode({
      channel: 0,
      kind: wire.KIND.open,
      payload: { protocol: 1, extension: 'client-starter', granted: ['workspace-read'] },
    });
    greeting.writeUInt16LE(1, 10);
    stream.end(greeting);
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
    const exit = await Promise.race([
      new Promise((resolve) => child.once('exit', (code, signal) => resolve({ code, signal }))),
      new Promise((_, reject) => setTimeout(() => reject(new Error('packed client starter did not reject an illegal greeting header')), 2_000)),
    ]);
    assert.deepEqual(exit, { code: 1, signal: null });
    assert.equal(spoke, false, 'illegal greeting header must be rejected before the starter sends a request');
    assert.equal(stdout, '');
    assert.equal(stderr, 'client-starter: startup failed: frame reserved bytes must be zero\n');
  } finally {
    peer?.destroy();
    if (child.exitCode === null) child.kill('SIGKILL');
    await new Promise((resolve) => server.close(resolve));
  }
}

async function runPackedGreetingTimeout(starter) {
  const socket = path.join(starter, 'host-greeting-timeout.sock');
  let spoke = false;
  let peer;
  const server = net.createServer((stream) => {
    peer = stream;
    stream.on('data', () => { spoke = true; });
  });
  await new Promise((resolve, reject) => { server.once('error', reject); server.listen(socket, resolve); });
  const child = spawn(process.execPath, ['main.js'], {
    cwd: starter,
    env: { ...process.env, HUSKLET_EXTENSION_SOCKET: socket, HUSKLET_EXTENSION_CONNECT_TIMEOUT_MS: '40' },
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  let stdout = ''; let stderr = '';
  child.stdout.setEncoding('utf8'); child.stdout.on('data', (chunk) => { stdout += chunk; });
  child.stderr.setEncoding('utf8'); child.stderr.on('data', (chunk) => { stderr += chunk; });
  try {
    const exit = await Promise.race([
      new Promise((resolve) => child.once('exit', (code, signal) => resolve({ code, signal }))),
      new Promise((_, reject) => setTimeout(() => reject(new Error('packed client starter did not enforce its greeting deadline')), 2_000)),
    ]);
    assert.deepEqual(exit, { code: 1, signal: null });
    assert.equal(spoke, false, 'silent host must receive no request before the greeting deadline');
    assert.equal(stdout, '');
    assert.equal(stderr, 'client-starter: startup failed: extension host handshake timed out after 40ms\n');
  } finally {
    peer?.destroy();
    if (child.exitCode === null) child.kill('SIGKILL');
    await new Promise((resolve) => server.close(resolve));
  }
}

async function runPackedMalformedReply(starter, installedClient) {
  const wire = await import(new URL('src/wire.js', `file://${installedClient}/`));
  const socket = path.join(starter, 'host-malformed-reply.sock');
  let requested = false;
  let peer;
  const server = net.createServer((stream) => {
    peer = stream;
    const reader = new wire.Reader();
    stream.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.channel !== 2 || frame.kind !== wire.KIND.request) continue;
        assert.deepEqual(frame.payload, { call: 'workspace_info' });
        requested = true;
        stream.end(wire.encode({
          channel: 2,
          kind: wire.KIND.response,
          payload: Buffer.from('{', 'utf8'),
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
    const exit = await Promise.race([
      new Promise((resolve) => child.once('exit', (code, signal) => resolve({ code, signal }))),
      new Promise((_, reject) => setTimeout(() => reject(new Error('packed client starter did not reject a malformed reply')), 2_000)),
    ]);
    assert.deepEqual(exit, { code: 1, signal: null });
    assert.equal(requested, true, 'packed starter must cross the handshake and issue its exact workspace request');
    assert.equal(stdout, '', 'malformed reply must not become invented workspace output');
    assert.match(stderr, /^client-starter: host connection ended: frame payload is not valid UTF-8 JSON: .{1,256}\n$/);
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
    'src/session.js', 'src/wire.js', 'examples/starter/.dockerignore', 'examples/starter/Dockerfile',
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
  const packedReadme = fs.readFileSync(path.join(installedClient, 'README.md'), 'utf8');
  assert.match(packedReadme, /no separate published client-only Husklet base image/);
  assert.match(packedReadme, /image build performs no npm registry resolution/);
  assert.match(packedReadme, /offline OCI build still requires the pinned Node base/);
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
  const starterDockerignore = fs.readFileSync(path.join(starter, '.dockerignore'), 'utf8');
  assert.equal(starterManifest.engines.node, manifest.engines.node, 'starter Node requirement drifted from the installed client');
  assert.equal(starterDockerignore,
    'node_modules/*\n!node_modules/@husklet/\nnode_modules/@husklet/*\n!node_modules/@husklet/client/\nnpm-debug.log*\n.git\n.gitignore\n',
    'starter image context must include only the installed client SDK from node_modules');
  assert.equal(starterManifest.dependencies['@husklet/client'], manifest.version);
  assert.equal(starterManifest.scripts.test, 'node --check main.js && node --input-type=module --eval "await Promise.all([import(\'@husklet/client\'), import(\'@husklet/client/protocol\')])"');
  execFileSync('npm', ['install', '--ignore-scripts', '--no-save', '--offline', '--no-audit', '--no-fund', path.join(scratch, packed.filename)], {
    cwd: starter, stdio: 'pipe',
  });
  execFileSync('npm', ['test'], { cwd: starter, stdio: 'pipe' });
  const withoutSocket = { ...process.env };
  delete withoutSocket.HUSKLET_EXTENSION_SOCKET;
  const missingSocket = spawnSync('npm', ['start', '--silent'], {
    cwd: starter, env: withoutSocket, encoding: 'utf8',
  });
  assert.equal(missingSocket.status, 1);
  assert.equal(missingSocket.signal, null);
  assert.equal(missingSocket.stdout, '');
  assert.equal(missingSocket.stderr, 'client-starter: startup failed: HUSKLET_EXTENSION_SOCKET is not set; an extension runs inside a workspace\n');
  const dockerfile = fs.readFileSync(path.join(starter, 'Dockerfile'), 'utf8');
  assert.match(dockerfile, /^ARG NODE_IMAGE=node:22-alpine@sha256:[0-9a-f]{64}$/m);
  assert.match(dockerfile, /^FROM \$\{NODE_IMAGE\}$/m);
  assert(!dockerfile.includes('--platform='), 'framework-neutral starter must inherit the selected image architecture');
  assert.match(dockerfile, /COPY --chown=node:node package\.json/);
  assert.match(dockerfile, /COPY --chown=node:node node_modules\/@husklet\/client \.\/node_modules\/@husklet\/client/);
  assert.match(dockerfile, /require\('@husklet\/client\/package\.json'\)\.version/);
  assert.doesNotMatch(dockerfile, /^RUN npm (?:ci|install)/m, 'image build must not resolve the npm registry');
  assert.match(dockerfile, /^USER node$/m);
  assert.match(dockerfile, /LABEL husklet\.extension\.manifest="\/etc\/husklet\/extension\.toml"/);
  const starterClient = path.join(starter, 'node_modules/@husklet/client');
  for (const architecture of ['x86_64', 'aarch64']) {
    await runPackedStarter(starter, starterClient, architecture, 'SIGTERM');
    await runPackedStarter(starter, starterClient, architecture, 'SIGINT');
    await runPackedStarter(starter, starterClient, architecture, 'SIGTERM', true);
  }
  await runPackedProtocolRefusal(starter, starterClient);
  await runPackedTruncatedGreeting(starter, starterClient);
  await runPackedOversizedGreeting(starter, starterClient);
  await runPackedIllegalHeader(starter, starterClient);
  await runPackedGreetingTimeout(starter);
  await runPackedMalformedReply(starter, starterClient);

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
      type WorkspaceConfiguration, type WorkspaceInfo,
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
    const rawInfo: Promise<{ reply: 'workspace'; with: WorkspaceInfo }> = session.call('workspace_info');
    const rawInspect = session.call('container_inspect', { id: 'a'.repeat(64) });
    // @ts-expect-error generated request payload rejects a missing immutable container ID
    session.call('container_inspect', {});
    // @ts-expect-error unknown calls are not part of the authoritative Rust request union
    session.call('invented_operation');
    const event: WireUiEvent = { interaction: 'key', trigger: 'Key', node: 1, id: 'key-1', key: 'Enter', keycode: 13, modifiers: 0, pressed: true };
    const protocol: 1 = PROTOCOL_VERSION;
    const lifecycle: ConnectOptions = { connectTimeout: 5_000, onClose: (error) => { void error.message; } };
    void configuration; void container; void processes; void topology; void text; void input;
    void acted; void xml; void request; void rawInfo; void rawInspect; void event; void protocol; void lifecycle; void cancellable.info();
  `);
  execFileSync(path.resolve(root, '../../node_modules/.bin/tsc'), [
    '--noEmit', '--strict', '--target', 'ES2022',
    '--module', 'NodeNext', '--moduleResolution', 'NodeNext', 'consumer.ts',
  ], { cwd: consumer, stdio: 'pipe' });
} finally {
  fs.rmSync(scratch, { recursive: true, force: true });
}

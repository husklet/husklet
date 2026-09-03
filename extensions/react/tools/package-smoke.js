import assert from 'node:assert/strict';
import { execFileSync, spawn } from 'node:child_process';
import fs from 'node:fs';
import net from 'node:net';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');
const scratch = fs.mkdtempSync(path.join(os.tmpdir(), 'husklet-react-pack-'));

async function runPackedStarter(consumer, starter, signal, hostEof = false) {
  const socket = path.join(consumer, `starter-${signal}${hostEof ? '-eof' : ''}.sock`);
  const wire = await import(new URL('src/wire.js', `file://${path.join(consumer, 'node_modules/@husklet/react/')}`));
  const calls = [];
  let peer;
  const server = net.createServer((stream) => {
    peer = stream;
    const reader = new wire.Reader();
    stream.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.channel === 0 || frame.kind !== wire.KIND.request) continue;
        calls.push(frame.payload);
        stream.write(wire.encode({
          channel: frame.channel,
          kind: wire.KIND.response,
          payload: frame.payload.call === 'interface_open_tab'
            ? { reply: 'identity', with: 'packed-starter' }
            : { reply: 'done' },
        }), () => {
          if (hostEof && frame.payload.call === 'interface_render_at') stream.destroy();
        });
      }
    });
    stream.write(wire.encode({
      channel: 0,
      kind: wire.KIND.open,
      payload: { protocol: 1, extension: 'react-starter', granted: ['interface'] },
    }));
  });
  await new Promise((resolve, reject) => {
    server.once('error', reject);
    server.listen(socket, resolve);
  });
  const child = spawn(process.execPath, ['main.js'], {
    cwd: starter,
    env: { ...process.env, HUSKLET_EXTENSION_SOCKET: socket },
    stdio: ['ignore', 'ignore', 'pipe'],
  });
  let stderr = '';
  let exit;
  child.stderr.setEncoding('utf8');
  child.stderr.on('data', (chunk) => (stderr += chunk));
  try {
    for (let attempt = 0; attempt < 400 && !calls.some(({ call }) => call === 'interface_render_at'); attempt += 1) {
      if (child.exitCode !== null) break;
      await new Promise((resolve) => setTimeout(resolve, 5));
    }
    assert.deepEqual(calls[0], { call: 'interface_open_tab', with: { title: 'React starter' } });
    const rendered = calls.find(({ call }) => call === 'interface_render_at');
    assert(rendered, `packed starter did not render through a real socket; stderr=${stderr}`);
    assert.equal(rendered.with.slot, 'packed-starter');
    assert.equal(rendered.with.frame.sequence, 1);
    assert(rendered.with.frame.patches.some((patch) => patch.SetProp?.value?.Text === 'Increment'));
    if (hostEof) {
      exit = await Promise.race([
        new Promise((resolve) => child.once('exit', (code, receivedSignal) => resolve({ code, signal: receivedSignal }))),
        new Promise((_, reject) => setTimeout(() => reject(new Error('packed React starter did not stop after host EOF')), 2_000)),
      ]);
    } else {
      assert.equal(stderr, '');
    }
  } finally {
    if (child.exitCode === null) child.kill(signal);
    exit ??= child.exitCode === null
      ? await new Promise((resolve) => child.once('exit', (code, signal) => resolve({ code, signal })))
      : { code: child.exitCode, signal: child.signalCode };
    peer?.destroy();
    await new Promise((resolve) => server.close(resolve));
  }
  assert.deepEqual(exit, hostEof ? { code: 1, signal: null } : { code: 0, signal: null },
    `packed React starter did not stop cleanly; stderr=${stderr}`);
  assert.equal(stderr, hostEof
    ? 'react-starter: host connection ended: extension host connection closed\n'
    : '');
}

async function runPackedStarterDenied(consumer, starter) {
  const socket = path.join(consumer, 'starter-interface-denied.sock');
  const wire = await import(new URL('src/wire.js', `file://${path.join(consumer, 'node_modules/@husklet/react/')}`));
  const requests = [];
  let peer;
  const server = net.createServer((stream) => {
    peer = stream;
    const reader = new wire.Reader();
    stream.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.kind === wire.KIND.request) requests.push(frame.payload);
      }
    });
    stream.write(wire.encode({
      channel: 0,
      kind: wire.KIND.open,
      payload: { protocol: 1, extension: 'react-starter', granted: [] },
    }));
  });
  await new Promise((resolve, reject) => { server.once('error', reject); server.listen(socket, resolve); });
  const child = spawn(process.execPath, ['main.js'], {
    cwd: starter,
    env: { ...process.env, HUSKLET_EXTENSION_SOCKET: socket },
    stdio: ['ignore', 'ignore', 'pipe'],
  });
  let stderr = '';
  child.stderr.setEncoding('utf8');
  child.stderr.on('data', (chunk) => { stderr += chunk; });
  try {
    const exit = await Promise.race([
      new Promise((resolve) => child.once('exit', (code, signal) => resolve({ code, signal }))),
      new Promise((_, reject) => setTimeout(() => reject(new Error('packed React starter did not report denied interface authority')), 2_000)),
    ]);
    assert.deepEqual(exit, { code: 1, signal: null });
    assert.deepEqual(requests, [], 'denied starter must not send an unauthorized interface request');
    assert.equal(stderr, 'react-starter: startup failed: extension lacks negotiated capability interface\n');
  } finally {
    peer?.destroy();
    if (child.exitCode === null) child.kill('SIGKILL');
    await new Promise((resolve) => server.close(resolve));
  }
}

function packageStageFiles(dockerfile, destination) {
  const packageStage = dockerfile.split(/^FROM \$\{NODE_IMAGE\}$/m, 1)[0];
  for (const line of packageStage.matchAll(/^COPY ([^\n]+)$/gm)) {
    const fields = line[1].trim().split(/\s+/);
    const target = fields.pop();
    assert(!fields.some((field) => field.startsWith('--')), 'package-stage COPY must remain directly reproducible');
    for (const source of fields) {
      const output = target === './'
        ? path.join(destination, path.basename(source))
        : path.join(destination, target.replace(/^\.\//, ''));
      fs.cpSync(path.join(root, '..', source), output, { recursive: true });
    }
  }
}

try {
  const dryRun = JSON.parse(execFileSync('npm', ['pack', '--dry-run', '--json', '--ignore-scripts'], {
    cwd: root, encoding: 'utf8',
  }));
  const names = new Set(dryRun[0].files.map(({ path: name }) => name));
  for (const required of [
    'package.json', 'README.md', 'LICENSE', 'catalogue.json', 'src/index.js', 'src/index.d.ts',
    'examples/starter/Dockerfile', 'examples/starter/extension.toml', 'examples/starter/main.js',
    'examples/starter/package.json',
  ]) {
    assert(names.has(required), `npm package omits ${required}`);
  }
  assert(!names.has('Dockerfile'), 'context-dependent base Dockerfile must not masquerade as a standalone npm artifact');
  assert(![...names].some((name) => name.startsWith('test/') || name.startsWith('tools/')), 'developer-only files leaked into package');

  const tarball = execFileSync('npm', ['pack', '--json', '--ignore-scripts', '--pack-destination', scratch], {
    cwd: root, encoding: 'utf8',
  });
  const filename = JSON.parse(tarball)[0].filename;
  const clientTarball = JSON.parse(execFileSync('npm', ['pack', '--json', '--ignore-scripts', '--pack-destination', scratch], {
    cwd: path.join(root, '..', 'client'), encoding: 'utf8',
  }))[0].filename;
  const isolatedCache = path.join(scratch, 'npm-cache');
  const imageRuntime = path.join(scratch, 'image-runtime');
  fs.cpSync(path.join(root, 'image-runtime'), imageRuntime, { recursive: true });
  execFileSync('npm', ['ci', '--ignore-scripts', '--omit=dev', '--no-audit', '--no-fund', '--cache', isolatedCache], {
    cwd: imageRuntime, stdio: 'pipe',
  });
  const consumer = path.join(scratch, 'consumer');
  fs.mkdirSync(consumer);
  fs.writeFileSync(path.join(consumer, 'package.json'), JSON.stringify({ private: true, type: 'module' }));
  execFileSync('npm', ['install', '--ignore-scripts', '--no-audit', '--no-fund', path.join(scratch, clientTarball), path.join(scratch, filename), 'react@18.3.1'], {
    cwd: consumer, stdio: 'pipe',
  });
  const runtime = execFileSync(process.execPath, ['--input-type=module', '--eval', `
    import { Button, CommandPaletteView, ConfirmAction, ResourceState, TerminalTranscript, acceptsChildren, connect, tags, workspace } from '@husklet/react';
    import catalogue from '@husklet/react/catalogue' with { type: 'json' };
    if (typeof connect !== 'function' || typeof workspace !== 'function' || typeof TerminalTranscript !== 'function' || typeof CommandPaletteView !== 'function' || typeof ConfirmAction !== 'function' || typeof ResourceState !== 'function') process.exit(1);
    if (Button !== 'Button' || !acceptsChildren('Column')) process.exit(2);
    if (catalogue.tags.length !== tags.length || catalogue.tags[0].name !== tags[0]) process.exit(3);
  `], { cwd: consumer, encoding: 'utf8' });
  assert.equal(runtime, '');
  const manifest = JSON.parse(fs.readFileSync(path.join(consumer, 'node_modules/@husklet/react/package.json'), 'utf8'));
  for (const field of ['cpu', 'os', 'libc']) {
    assert.equal(manifest[field], undefined, `React SDK gained an architecture restriction in ${field}`);
  }
  assert.equal(manifest.dependencies['@husklet/client'], manifest.version, 'React SDK must depend on the same public client version');
  assert.equal(manifest.exports['.'].types, './src/index.d.ts');

  const installedStarter = path.join(consumer, 'node_modules/@husklet/react/examples/starter');
  const starterPackage = JSON.parse(fs.readFileSync(path.join(installedStarter, 'package.json'), 'utf8'));
  const starterDockerfile = fs.readFileSync(path.join(installedStarter, 'Dockerfile'), 'utf8');
  const starterManifest = fs.readFileSync(path.join(installedStarter, 'extension.toml'), 'utf8');
  execFileSync(process.execPath, ['--check', path.join(installedStarter, 'main.js')], { stdio: 'pipe' });
  assert.equal(starterPackage.private, true);
  assert.equal(starterPackage.type, 'module');
  assert.equal(starterPackage.scripts.start, 'node main.js');
  assert.equal(starterPackage.scripts.test, 'node --check main.js && node --input-type=module --eval "await Promise.all([import(\'@husklet/client\'), import(\'@husklet/react\'), import(\'react\')])"');
  assert.equal(starterPackage.dependencies['@husklet/react'], manifest.version);
  assert.equal(starterPackage.dependencies['@husklet/client'], manifest.version);
  assert.equal(starterPackage.dependencies.react, '18.3.1');
  assert.match(
    starterDockerfile,
    new RegExp(`^ARG HUSKLET_REACT_IMAGE=ghcr\\.io/husklet/husklet/extension-react-base:${manifest.version.replaceAll('.', '\\.')}$`, 'm'),
  );
  assert.match(starterDockerfile, /^FROM \$\{HUSKLET_REACT_IMAGE\}$/m);
  assert.match(starterDockerfile, /COPY --chown=node:node main\.js \/app\/main\.js/);
  assert.match(starterDockerfile, /COPY --chown=node:node extension\.toml \/etc\/husklet\/extension\.toml/);
  assert.match(starterDockerfile, /LABEL husklet\.extension\.manifest="\/etc\/husklet\/extension\.toml"/);
  const standaloneStarter = path.join(scratch, 'standalone-starter');
  fs.cpSync(installedStarter, standaloneStarter, { recursive: true });
  execFileSync('npm', [
    'install', '--ignore-scripts', '--no-audit', '--no-fund',
    path.join(scratch, clientTarball), path.join(scratch, filename), 'react@18.3.1',
  ], { cwd: standaloneStarter, stdio: 'pipe' });
  execFileSync('npm', ['test'], { cwd: standaloneStarter, stdio: 'pipe' });
  const starterLock = JSON.parse(fs.readFileSync(path.join(standaloneStarter, 'package-lock.json'), 'utf8'));
  assert.equal(starterLock.packages['node_modules/@husklet/react'].version, manifest.version);
  assert.equal(starterLock.packages['node_modules/@husklet/client'].version, manifest.version);
  assert.match(starterLock.packages['node_modules/@husklet/react'].resolved, /^file:/);
  await runPackedStarter(consumer, standaloneStarter, 'SIGTERM');
  await runPackedStarter(consumer, standaloneStarter, 'SIGINT');
  await runPackedStarter(consumer, standaloneStarter, 'SIGTERM', true);
  await runPackedStarterDenied(consumer, standaloneStarter);
  assert(!starterDockerfile.includes('--platform='), 'starter must inherit the selected image architecture');
  assert(!/^USER root$/m.test(starterDockerfile), 'starter must not regain root after the base drops privileges');
  assert.match(starterManifest, /^name = "react-starter"$/m);
  assert.match(starterManifest, new RegExp(`^version = "${manifest.version.replaceAll('.', '\\.')}"$`, 'm'));
  assert.match(starterManifest, /^protocol = 1$/m);
  assert.match(starterManifest, /^capabilities = \["interface"\]$/m);

  fs.writeFileSync(path.join(consumer, 'consumer.ts'), `
    import { CommandPaletteView, ConfirmAction, TerminalTranscript, render, useHostEvents, usePaneSelection, workspace, type CommandPaletteViewProps, type ConfirmActionProps, type ExtensionCapability, type HostEvent, type InterfaceEvent, type InterfaceSourceMutation, type Session, type ProcessList, type TerminalTranscriptProps } from '@husklet/react';
    declare const session: Session;
    const api = workspace(session);
    const cancellable = api.withSignal(new AbortController().signal);
    const table: Promise<ProcessList> = api.containers.processes('container');
    const attachmentGrant: ExtensionCapability = 'container-attach';
    void attachmentGrant;
    void table;
    void cancellable.info();
    void api.containers.exec('container', { command: ['sh'], workingDirectory: '/work' });
    void api.containers.create({ image: 'alpine:3.20', name: 'worker', command: ['sleep', '10'], environment: [['MODE', 'test']], mounts: [{ volume: 'cache', target: '/cache', read_only: true }], ports: [{ container: 8080, host: null, protocol: 'tcp' }], memory_mb: 256, cpus: 2, pids_limit: 128 });
    void api.containers.executions();
    void api.containers.executionLogs('a'.repeat(32), { stderr: false });
    void api.containers.waitExecution('a'.repeat(32), { timeoutMs: 50 });
    void api.containers.removeExecution('a'.repeat(32));
    void api.containers.attachTerminal('a'.repeat(64), ['sh']);
    void api.images.startPull('alpine:3.20');
    void api.images.pullStatus('pull-1');
    void api.images.cancelPull('pull-1');
    void api.files.stat('project/package.json');
    void api.containers.rename('a'.repeat(64), 'worker_2.prod');
    void api.terminal.retitle('pane-1', 'Build 🧪');
    const transcriptProps: TerminalTranscriptProps = { lines: [{ text: 'ready', stream: 'stdout' }], cursor: { line: 1, column: 5 } };
    void TerminalTranscript; void transcriptProps;
    const paletteProps: CommandPaletteViewProps = { commands: [{ id: 'open', title: 'Open terminal', group: 'Workspace' }] };
    void CommandPaletteView; void paletteProps;
    const confirmationProps: ConfirmActionProps = { authorityKey: 'volume:cache:g7', label: 'Remove', confirmLabel: 'Confirm remove', question: 'Remove cache?', onConfirm: async (authority) => { void authority; } };
    void ConfirmAction; void confirmationProps;
    void api.subscribe('terminal');
    void api.subscribe('volumes');
    void api.subscribe('extensions');
    void api.watchExtensions((extensions) => { void extensions[0]?.name; });
    void api.watchExtensionAcquisitions((change) => { void change.revision; });
    void api.watchContainers((containers) => { void containers[0]?.id; });
    void api.watchExecutions((executions) => { void executions.truncated; });
    void api.watchImagePulls((change) => { void change.job; });
    void api.watchWorkspaceLifecycle((change) => { void change.action; });
    void api.watchWorkspaceEvents((batch) => { void batch.dropped; });
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
          const x: number | null | undefined = received.x;
          return phase + ':' + String(x);
        }
        case 'select': return received.rows.join(',');
        case 'edit': return received.row.id + ':' + received.column + '=' + received.value;
        case 'sort': return received.column + ':' + String(received.descending);
        case 'scroll': return received.dx + ',' + received.dy;
        case 'context': return received.x + ',' + received.y;
        case 'focus': return String(received.focused);
        case 'change': return String(received.value);
        case 'drag': return received.id;
        case 'drop': return received.source + ':' + received.x + ',' + received.y;
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
  assert.match(dockerfile, /^ARG NODE_IMAGE=node:22-alpine@sha256:[0-9a-f]{64}$/m);
  assert.match(dockerfile, /^ARG NODE_VERSION=22\.23\.2$/m);
  assert.match(dockerfile, /^ARG NPM_VERSION=10\.9\.8$/m);
  assert.match(dockerfile, /FROM \$\{NODE_IMAGE\} AS package/);
  assert.match(dockerfile, /npm pack --ignore-scripts/);
  assert.match(dockerfile, /COPY react\/image-runtime\/package\.json react\/image-runtime\/package-lock\.json \.\//);
  assert.match(dockerfile, /npm ci --ignore-scripts --omit=dev/);
  assert.match(dockerfile, /tar -xzf \/opt\/husklet-sdk\/husklet-client-\*\.tgz --strip-components=1/);
  assert.match(dockerfile, /tar -xzf \/opt\/husklet-sdk\/husklet-react-\*\.tgz --strip-components=1/);
  assert(dockerfile.includes('sed -i "s/^version = .*/version = \\"${HUSKLET_REACT_VERSION}\\"/" react/examples/starter/extension.toml'));
  assert.match(dockerfile, /^USER node$/m);
  assert.match(dockerfile, /test "\$\(node --version\)" = "v\$\{NODE_VERSION\}"/);
  assert.match(dockerfile, /test "\$\(npm --version\)" = "\$\{NPM_VERSION\}"/);
  assert.match(dockerfile, /HUSKLET_EXTENSION_SOCKET=\/run\/husklet\/extension\.sock/);
  assert(!dockerfile.includes('--platform='), 'base image must not pin one architecture');
  assert.match(readme, /npm install @husklet\/react react@18\.3\.1/);
  assert.match(readme, /one published Husklet SDK base image/);
  assert.match(readme, /repository's `extensions\/react\/Dockerfile` is release infrastructure/);
  assert.match(readme, /complete `examples\/starter` Docker context/);
  assert.match(readme, /pin that argument to a\s+registry digest/);
  assert.match(readme, /offline OCI build still requires.*base image to\s+already exist/s);
  assert(!readme.includes('immutable base-image tag'), 'a mutable version tag must not be documented as immutable');
  assert.match(readme, /examples\/starter/);
  assert.match(readme, /render\(React\.createElement\(App\), session/);
  assert(!readme.includes('```jsx'), 'Node-only starter documentation must not require a JSX transform');
  assert.match(readme, /host\.update\('backend', configuration\.generation,/);
  assert(!readme.includes('husklet.extension.manifest="{...}"'), 'README must not suggest an inline manifest');

  // Reproduce the first Docker stage without an OCI builder or a registry. Its
  // npm package must be the same SDK a clean npm consumer receives; otherwise
  // an export can pass package tests and still be absent from the base image.
  const baseSource = path.join(scratch, 'base-package-source');
  const baseOutput = path.join(scratch, 'base-package-output');
  fs.mkdirSync(baseSource);
  fs.mkdirSync(baseOutput);
  packageStageFiles(dockerfile, baseSource);
  execFileSync('npm', ['pkg', 'set', 'version=9.8.7'], { cwd: path.join(baseSource, 'client'), stdio: 'pipe' });
  execFileSync('npm', ['pkg', 'set', 'version=9.8.7'], { cwd: path.join(baseSource, 'react'), stdio: 'pipe' });
  execFileSync('npm', ['pkg', 'set', 'dependencies.@husklet/client=9.8.7'], { cwd: path.join(baseSource, 'react'), stdio: 'pipe' });
  const baseStarterManifest = path.join(baseSource, 'react/examples/starter/extension.toml');
  fs.writeFileSync(baseStarterManifest, fs.readFileSync(baseStarterManifest, 'utf8')
    .replace(/^version = .*$/m, 'version = "9.8.7"'));
  assert.match(fs.readFileSync(baseStarterManifest, 'utf8'), /^version = "9\.8\.7"$/m);
  const basePack = JSON.parse(execFileSync('npm', [
    'pack', '--json', '--ignore-scripts', '--pack-destination', baseOutput,
  ], { cwd: path.join(baseSource, 'react'), encoding: 'utf8' }));
  const baseNames = new Set(basePack[0].files.map(({ path: name }) => name));
  assert.deepEqual(baseNames, names, 'base image must install the complete published SDK package');
  const baseClient = JSON.parse(execFileSync('npm', [
    'pack', '--json', '--ignore-scripts', '--pack-destination', baseOutput,
  ], { cwd: path.join(baseSource, 'client'), encoding: 'utf8' }))[0];
  assert(baseClient.files.some(({ path: name }) => name === 'src/index.js'), 'base image must include the framework-neutral client');
  assert(baseClient.files.some(({ path: name }) => name === 'src/generated-protocol.js'), 'base image must include generated protocol validation');
  assert(baseClient.files.some(({ path: name }) => name === 'src/generated-protocol.d.ts'), 'base image must include generated protocol types');
  const repeatedOutput = path.join(scratch, 'base-package-output-repeat');
  fs.mkdirSync(repeatedOutput);
  const repeatedReact = JSON.parse(execFileSync('npm', [
    'pack', '--json', '--ignore-scripts', '--pack-destination', repeatedOutput,
  ], { cwd: path.join(baseSource, 'react'), encoding: 'utf8' }))[0];
  const repeatedClient = JSON.parse(execFileSync('npm', [
    'pack', '--json', '--ignore-scripts', '--pack-destination', repeatedOutput,
  ], { cwd: path.join(baseSource, 'client'), encoding: 'utf8' }))[0];
  assert.equal(repeatedReact.integrity, basePack[0].integrity, 'React SDK tarball assembly is not reproducible');
  assert.equal(repeatedClient.integrity, baseClient.integrity, 'client SDK tarball assembly is not reproducible');
  const locked = fs.readFileSync(path.join(imageRuntime, 'package-lock.json'));
  for (const [scope, archive] of [['client', baseClient.filename], ['react', basePack[0].filename]]) {
    const destination = path.join(imageRuntime, 'node_modules/@husklet', scope);
    fs.mkdirSync(destination, { recursive: true });
    execFileSync('tar', ['-xzf', path.join(baseOutput, archive), '--strip-components=1', '-C', destination], { stdio: 'pipe' });
  }
  assert.deepEqual(fs.readFileSync(path.join(imageRuntime, 'package-lock.json')), locked, 'offline SDK install changed the committed third-party lock');
  execFileSync(process.execPath, ['--input-type=module', '--eval', `
    import { Session as ClientSession } from '@husklet/client';
    import { Session, connect, render } from '@husklet/react';
    if (Session !== ClientSession || typeof connect !== 'function' || typeof render !== 'function') process.exit(1);
  `], { cwd: imageRuntime, stdio: 'pipe' });
} finally {
  fs.rmSync(scratch, { recursive: true, force: true });
}

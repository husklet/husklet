import assert from 'node:assert/strict';
import net from 'node:net';
import test from 'node:test';

import { ExtensionError, Session, protocolCoverage, workspace } from '../src/index.js';
import { KIND, Reader, encode } from '../src/wire.js';
import { PROTOCOL } from '../src/session.js';

async function pair(options) {
  const server = net.createServer();
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
  const address = server.address();
  const accepted = new Promise((resolve) => server.once('connection', resolve));
  const connecting = new Promise((resolve, reject) => {
    const socket = net.createConnection(address.port, '127.0.0.1');
    socket.once('error', reject);
    socket.once('connect', () => resolve(socket));
  });
  const [host, extension] = await Promise.all([accepted, connecting]);
  host.write(encode({ channel: 0, kind: KIND.request, payload: { protocol: PROTOCOL, extension: 'test', granted: [] } }));
  const session = new Session(extension, options);
  await session.ready;
  return { host, session, server };
}

function frames(stream) {
  const reader = new Reader();
  const queued = [];
  const waiters = [];
  stream.on('data', (chunk) => {
    for (const frame of reader.take(chunk)) {
      const waiter = waiters.shift();
      if (waiter) waiter(frame);
      else queued.push(frame);
    }
  });
  return () => new Promise((resolve) => {
    const frame = queued.shift();
    if (frame) resolve(frame);
    else waiters.push(resolve);
  });
}

test('ordered replies correlate concurrent typed calls and failures reject', async () => {
  const stage = await pair();
  const next = frames(stage.host);
  await next(); // hello
  const api = workspace(stage.session);
  const info = api.info();
  const list = api.containers.list();
  assert.equal((await next()).payload.call, 'workspace_info');
  assert.equal((await next()).payload.call, 'container_list');
  stage.host.write(encode({ channel: 2, kind: KIND.response, payload: { reply: 'workspace', with: { name: 'dev', architecture: 'arm64', image: 'alpine' } } }));
  stage.host.write(encode({ channel: 2, kind: KIND.response, flags: 3, payload: { error: 'denied', capability: 'container-read', detail: 'not granted' } }));
  assert.equal((await info).name, 'dev');
  await assert.rejects(list, (error) => error instanceof ExtensionError && error.kind === 'denied');
  stage.session.close(); stage.host.destroy(); stage.server.close();
});

test('pending calls are bounded and a timeout closes the ambiguous ordered stream', async () => {
  const stage = await pair({ pendingLimit: 2, timeout: 100 });
  const next = frames(stage.host);
  await next();
  const first = stage.session.call('workspace_info');
  await new Promise((resolve) => setTimeout(resolve, 60));
  const second = stage.session.call('workspace_list');
  await assert.rejects(stage.session.call('container_list'), /limit/);
  await assert.rejects(first, /timed out/);
  await assert.rejects(second, /timed out/);
  await assert.rejects(stage.session.call('image_list'), /closed/);
  stage.session.close(); stage.host.destroy(); stage.server.close();
});

test('an event returns credit only after delivery', async () => {
  const seen = [];
  const stage = await pair({ onEvent: (event) => seen.push(event) });
  const next = frames(stage.host);
  await next();
  stage.host.write(encode({ channel: 4, kind: KIND.event, payload: { snapshot: 'containers', of: [] } }));
  const credit = await next();
  assert.deepEqual(seen, [{ snapshot: 'containers', of: [] }]);
  assert.equal(credit.channel, 4);
  assert.equal(credit.kind, KIND.credit);
  assert.equal(credit.payload, 1);
  stage.session.close(); stage.host.destroy(); stage.server.close();
});

test('coverage names deep-control protocol gaps without exposing fake methods', () => {
  assert.deepEqual(protocolCoverage.available.workspace, ['info', 'list']);
  assert.ok(protocolCoverage.available.containers.includes('create'));
  assert.ok(protocolCoverage.available.containers.includes('remove'));
  assert.ok(protocolCoverage.available.terminal.includes('read'));
  assert.ok(protocolCoverage.available.terminal.includes('split'));
  assert.ok(protocolCoverage.unavailable.workspace.includes('updateConfiguration'));
  assert.ok(protocolCoverage.unavailable.containers.includes('processes'));
  assert.ok(protocolCoverage.unavailable.terminal.includes('switchOccupant'));
  assert.ok(protocolCoverage.unavailable.events.includes('hostSnapshots'));
  assert.ok(protocolCoverage.unavailable.events.includes('keyboard'));
  const api = workspace({ call() { throw new Error('not called'); } });
  assert.equal(api.createWorkspace, undefined);
  assert.equal(api.containers.processes, undefined);
  assert.equal(api.terminal.writeInput, undefined);
});

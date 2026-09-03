import assert from 'node:assert/strict';
import { mkdtemp, rm } from 'node:fs/promises';
import net from 'node:net';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import { createElement as h } from 'react';
import { connect, workspace } from '../../react/src/index.js';
import { KIND, Reader, encode } from '../../react/src/wire.js';
import { WorkspaceManager } from '../src/app.js';
import { VOLUME_DETAIL_SOURCE, VolumeDetailsSource } from '../src/model.js';
import { host } from './host.js';

test('same-name volume generation replacement invalidates detail authority', { timeout: 5_000 }, async () => {
  const directory = await mkdtemp(join(tmpdir(), 'husklet-volume-detail-lifecycle-'));
  const socketPath = join(directory, 'host.sock');
  const name = 'cache'; const oldGeneration = 'a'.repeat(32); const newGeneration = 'b'.repeat(32);
  let lists = 0; let inspections = 0;
  const server = net.createServer((socket) => {
    const reader = new Reader();
    socket.write(encode({ channel: 0, kind: KIND.open, payload: { protocol: 1, extension: 'volume-detail-lifecycle-test', granted: ['volume-read', 'volume-write'] } }));
    socket.on('data', (chunk) => { for (const frame of reader.take(chunk)) {
      const call = frame.payload?.call; if (!call) continue;
      let payload = { reply: 'done' };
      if (call === 'volume_list') { lists += 1; payload = { reply: 'volumes', with: [{ name, driver: 'local', generation: lists === 1 ? oldGeneration : newGeneration }] }; }
      else if (call === 'volume_inspect') { inspections += 1; payload = { reply: 'volume', with: { name, driver: inspections === 1 ? 'old-driver' : 'new-driver', generation: inspections === 1 ? oldGeneration : newGeneration } }; }
      const delay = call === 'volume_inspect' && inspections === 1 ? 150 : 20;
      setTimeout(() => socket.write(encode({ channel: frame.channel, kind: KIND.response, flags: 1, payload })), delay);
    } });
  });
  await new Promise((resolve, reject) => server.listen(socketPath, (error) => error ? reject(error) : resolve()));
  let session; let inspectionSession; let stage;
  try {
    session = await connect({ path: socketPath }); inspectionSession = await connect({ path: socketPath });
    const framed = workspace(session); const inspectionApi = workspace(inspectionSession); const mutations = [];
    stage = host(); stage.render(h(WorkspaceManager, { api: { ...framed, volumes: { ...framed.volumes, inspect: inspectionApi.volumes.inspect }, subscribe: undefined, unsubscribe: undefined }, volumeDetails: new VolumeDetailsSource(async (mutation) => mutations.push(mutation)), initial: { containers: [], executions: [], images: [], networks: [] } }));
    invoke(stage, 'Volumes'); await until(() => labelled(stage, name));
    change(stage, 'Volume name', 'draft-volume');
    invoke(stage, 'Inspect'); await until(() => inspections === 1);
    invoke(stage, 'Remove'); assert.ok(labelled(stage, `Remove volume ${name} generation ${oldGeneration}?`));
    const start = stage.frames.length; invoke(stage, 'Refresh'); await until(() => labelled(stage, `Remove volume ${name} generation ${newGeneration}?`) === undefined && lists === 2);
    const patches = stage.frames.slice(start).flatMap((frame) => frame.patches);
    assert.ok(patches.some((patch) => 'Remove' in patch), 'replacement unmounts prior generation and consent');
    assert.equal(patches.some((patch) => patch.SetProp?.value?.Text === 'old-driver'), false);
    assert.equal(value(stage, 'Volume name'), 'draft-volume', 'independent creation input survives replacement');
    await new Promise((resolve) => setTimeout(resolve, 170));
    assert.equal(lengths(mutations).length, 0, 'late old-generation detail cannot publish');
    invoke(stage, 'Remove'); assert.ok(labelled(stage, `Remove volume ${name} generation ${newGeneration}?`));
    invoke(stage, 'Cancel');
    invoke(stage, 'Inspect'); await until(() => lengths(mutations).length === 1);
    assert.deepEqual(lengths(mutations)[0], { source: VOLUME_DETAIL_SOURCE, version: 1, rows: 2 });
    assert.equal(inspections, 2);
  } finally { stage?.render(null); await new Promise((resolve) => setTimeout(resolve, 30)); session?.close(); inspectionSession?.close(); await new Promise((resolve) => server.close(resolve)); await rm(directory, { recursive: true, force: true }); }
});

function lengths(mutations) { return mutations.flatMap((mutation) => mutation.Length ? [mutation.Length] : []); }
function labelled(stage, label) { return stage.frames.flatMap((frame) => frame.patches).filter((patch) => patch.SetProp?.prop === 'Label' && patch.SetProp.value?.Text === label).at(-1); }
function invoke(stage, label) { const nodes = stage.frames.flatMap((frame) => frame.patches).filter((patch) => patch.SetProp?.prop === 'Label' && patch.SetProp.value?.Text === label).map((patch) => patch.SetProp.id).reverse(); assert.ok(nodes.some((node) => stage.surface.dispatch({ trigger: 'Invoke', node, id: `${node}:Invoke`, value: null })), `${label} invokes`); }
function change(stage, placeholder, next) { const node = stage.frames.flatMap((frame) => frame.patches).filter((patch) => patch.SetProp?.prop === 'Placeholder' && patch.SetProp.value?.Text === placeholder).at(-1)?.SetProp.id; assert.ok(stage.surface.dispatch({ trigger: 'Change', node, id: `${node}:Change`, value: next })); }
function value(stage, placeholder) { const node = stage.frames.flatMap((frame) => frame.patches).filter((patch) => patch.SetProp?.prop === 'Placeholder' && patch.SetProp.value?.Text === placeholder).at(-1)?.SetProp.id; return stage.frames.flatMap((frame) => frame.patches).filter((patch) => patch.SetProp?.id === node && patch.SetProp.prop === 'Value').at(-1)?.SetProp.value?.Text; }
async function until(done) { const deadline = Date.now() + 2_000; while (!done()) { if (Date.now() >= deadline) throw new Error('volume detail lifecycle did not settle'); await new Promise((resolve) => setTimeout(resolve, 10)); } }

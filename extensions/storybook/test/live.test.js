import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import fs from 'node:fs';
import net from 'node:net';
import os from 'node:os';
import path from 'node:path';
import test from 'node:test';

import { PACKAGE } from './host.js';
import { FLOW_STORIES } from '../dist/app.js';
import { grouped, tags } from '../dist/catalogue.js';

const { KIND, Reader, encode } = await import(new URL('src/wire.js', `file://${PACKAGE}`));

function node(patches, tag, label) {
  let candidate = null;
  for (const patch of patches) {
    if (patch.Create?.tag === tag) candidate = patch.Create.id;
    if (candidate !== null && patch.SetProp?.id === candidate && patch.SetProp.prop === 'Label'
      && patch.SetProp.value.Text === label) return candidate;
  }
  return null;
}

function nodeWithProp(patches, tag, prop, expected) {
  const candidates = new Set();
  for (const patch of patches) {
    if (patch.Create?.tag === tag) candidates.add(patch.Create.id);
    if (patch.SetProp?.prop === prop && patch.SetProp.value?.Text === expected
      && candidates.has(patch.SetProp.id)) return patch.SetProp.id;
  }
  return null;
}

function apply(nodes, patches) {
  for (const patch of patches) {
    if (patch.Create) nodes.set(patch.Create.id, { tag: patch.Create.tag, props: new Map() });
    if (patch.SetProp) nodes.get(patch.SetProp.id)?.props.set(patch.SetProp.prop, patch.SetProp.value);
    if (patch.Remove) nodes.delete(patch.Remove.id);
  }
}

function liveNode(nodes, tag, label) {
  return [...nodes].find(([, candidate]) => candidate.tag === tag
    && candidate.props.get('Label')?.Text === label)?.[0] ?? null;
}

async function until(condition, message) {
  for (let attempt = 0; attempt < 400; attempt += 1) {
    const value = condition();
    if (value) return value;
    await new Promise((resolve) => setTimeout(resolve, 5));
  }
  throw new Error(message);
}

test('the shipped entrypoint connects and renders the complete playground over a real socket', async (context) => {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'husklet-storybook-live-'));
  const socket = path.join(directory, 'extension.sock');
  const calls = [];
  let accepted;
  const server = net.createServer((stream) => {
    accepted = stream;
    const reader = new Reader();
    stream.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.channel === 0) continue;
        if (frame.kind !== KIND.request) continue;
        calls.push(frame.payload);
        const payload = frame.payload.call === 'interface_open_tab'
          ? { reply: 'identity', with: 'storybook-main' }
          : { reply: 'done' };
        stream.write(encode({
          channel: frame.channel,
          kind: KIND.response,
          payload,
        }));
      }
    });
    stream.write(encode({
      channel: 0,
      kind: KIND.open,
      payload: { protocol: 1, extension: 'storybook', granted: ['interface:render'] },
    }));
  });
  await new Promise((resolve, reject) => {
    server.once('error', reject);
    server.listen(socket, resolve);
  });

  const child = spawn(process.execPath, ['dist/main.js'], {
    cwd: path.resolve(import.meta.dirname, '..'),
    env: { ...process.env, HUSKLET_EXTENSION_SOCKET: socket },
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  let stderr = '';
  child.stderr.setEncoding('utf8');
  child.stderr.on('data', (chunk) => (stderr += chunk));

  context.after(async () => {
    accepted?.destroy();
    if (child.exitCode === null) child.kill('SIGTERM');
    await new Promise((resolve) => child.once('exit', resolve));
    await new Promise((resolve) => server.close(resolve));
    fs.rmSync(directory, { recursive: true, force: true });
  });

  const rendered = await until(
    () => calls.find((call) => call.call === 'interface_render_at'),
    `storybook never rendered; stderr=${stderr}`,
  );
  const length = await until(
    () => calls.find((call) => call.call === 'source_resize_at'),
    `storybook never published its large source; stderr=${stderr}`,
  );
  assert.deepEqual(calls[0], { call: 'interface_open_tab', with: { title: 'Storybook' } });
  assert.equal(rendered.with.slot, 'storybook-main');
  assert.equal(rendered.with.frame.sequence, 1);
  assert.equal(length.with.slot, 'storybook-main');
  assert.deepEqual(length.with.mutation.Length, { source: 100, version: 1, rows: 100_000 });
  assert.ok(rendered.with.frame.patches.length < 1_200, 'the live frame exceeded the host patch budget');
  assert.equal(
    rendered.with.frame.patches.filter((patch) => patch.Create?.tag === 'ListItemButton').length,
    FLOW_STORIES.length + grouped().find((family) => family.name === 'buttons').tags.length,
    'the live playground did not render flows and the active component family',
  );
  assert.ok(
    rendered.with.frame.patches.some((patch) => patch.Create?.tag === 'Scroll'),
    'the live playground did not render its scrolling browser',
  );
  const live = new Map();
  apply(live, rendered.with.frame.patches);
  const search = nodeWithProp(rendered.with.frame.patches, 'Entry', 'Placeholder', 'Search flows and components');
  assert.ok(search, 'the live playground has no global component search');
  const selected = new Set();
  for (const tag of tags) {
    let before = calls.filter((call) => call.call === 'interface_render_at').length;
    accepted.write(encode({ channel: 2, kind: KIND.event, payload: {
      slot: 'storybook-main', event: 'Change', id: `${search}:Change`, node: search, value: tag.name,
    } }));
    const searchFrame = await until(
      () => calls.filter((call) => call.call === 'interface_render_at')[before],
      `searching for <${tag.name}> never crossed the socket; stderr=${stderr}`,
    );
    apply(live, searchFrame.with.frame.patches);
    assert.ok(searchFrame.with.frame.patches.length < 1_200, `<${tag.name}> search exceeded the patch budget`);
    const choice = liveNode(live, 'ListItemButton', tag.name);
    assert.ok(choice, `<${tag.name}> is not selectable from live global navigation`);

    before = calls.filter((call) => call.call === 'interface_render_at').length;
    accepted.write(encode({ channel: 2, kind: KIND.event, payload: {
      slot: 'storybook-main', event: 'Invoke', id: `${choice}:Invoke`, node: choice,
    } }));
    const componentFrame = await until(
      () => calls.filter((call) => call.call === 'interface_render_at')[before],
      `selecting <${tag.name}> never crossed the socket; stderr=${stderr}`,
    );
    apply(live, componentFrame.with.frame.patches);
    assert.ok(componentFrame.with.frame.patches.length < 1_200, `<${tag.name}> selection exceeded the patch budget`);
    assert.ok(
      componentFrame.with.frame.patches.some((patch) => patch.Create?.tag === tag.name),
      `selecting <${tag.name}> did not render that native component`,
    );
    selected.add(tag.name);
  }
  assert.deepEqual([...selected].sort(), tags.map((tag) => tag.name).sort());

  // Keyboard and pointer input use the same live event channel as selection.
  let renderCount = calls.filter((call) => call.call === 'interface_render_at').length;
  accepted.write(encode({ channel: 2, kind: KIND.event, payload: {
    slot: 'storybook-main', event: 'Change', id: `${search}:Change`, node: search, value: 'Button',
  } }));
  const buttonSearch = await until(
    () => calls.filter((call) => call.call === 'interface_render_at')[renderCount],
    `searching for the interactive Button never crossed the socket; stderr=${stderr}`,
  );
  apply(live, buttonSearch.with.frame.patches);
  const buttonChoice = liveNode(live, 'ListItemButton', 'Button');
  assert.ok(buttonChoice, 'Button is absent from live search results');
  renderCount = calls.filter((call) => call.call === 'interface_render_at').length;
  accepted.write(encode({ channel: 2, kind: KIND.event, payload: {
    slot: 'storybook-main', event: 'Invoke', id: `${buttonChoice}:Invoke`, node: buttonChoice,
  } }));
  const buttonFrame = await until(
    () => calls.filter((call) => call.call === 'interface_render_at')[renderCount],
    `selecting the interactive Button never crossed the socket; stderr=${stderr}`,
  );
  apply(live, buttonFrame.with.frame.patches);
  const previewButton = node(buttonFrame.with.frame.patches, 'Button', 'Button');
  assert.ok(previewButton, 'the selected Button preview is absent');
  for (const payload of [
    { event: 'Key', key: 'Enter', keycode: 36, pressed: true, modifiers: 0 },
    { event: 'Pointer', phase: 'press', x: 8, y: 5, button: 1, modifiers: 0 },
  ]) {
    renderCount = calls.filter((call) => call.call === 'interface_render_at').length;
    accepted.write(encode({ channel: 2, kind: KIND.event, payload: {
      slot: 'storybook-main', id: `${previewButton}:${payload.event}`, node: previewButton, ...payload,
    } }));
    const interaction = await until(
      () => calls.filter((call) => call.call === 'interface_render_at')[renderCount],
      `${payload.event} never returned from the extension; stderr=${stderr}`,
    );
    assert.ok(interaction.with.frame.patches.length < 64, `${payload.event} response exceeded its patch budget`);
    assert.ok(interaction.with.frame.patches.some((patch) =>
      patch.SetProp?.value?.Text?.includes(`${payload.event} received`)),
    `${payload.event} did not reach the visible bounded interaction console`);
    apply(live, interaction.with.frame.patches);
  }

  // Return to the composed-flow navigation before exercising its controls.
  renderCount = calls.filter((call) => call.call === 'interface_render_at').length;
  accepted.write(encode({ channel: 2, kind: KIND.event, payload: {
    slot: 'storybook-main', event: 'Change', id: `${search}:Change`, node: search, value: '',
  } }));
  const cleared = await until(
    () => calls.filter((call) => call.call === 'interface_render_at')[renderCount],
    `clearing component search never crossed the socket; stderr=${stderr}`,
  );
  apply(live, cleared.with.frame.patches);
  const story = liveNode(live, 'ListItemButton', 'Container operations console');
  assert.ok(story, 'container operations is not selectable from the live sidebar');
  renderCount = calls.filter((call) => call.call === 'interface_render_at').length;
  accepted.write(encode({ channel: 2, kind: KIND.event, payload: {
    slot: 'storybook-main', event: 'Invoke', id: `${story}:Invoke`, node: story,
  } }));
  const storyFrame = await until(
    () => calls.filter((call) => call.call === 'interface_render_at')[renderCount],
    `container operations never crossed the socket; stderr=${stderr}`,
  );
  const inspect = node(storyFrame.with.frame.patches, 'Button', 'Inspect processes and logs');
  assert.ok(inspect, 'container operations has no inspection action');
  renderCount = calls.filter((call) => call.call === 'interface_render_at').length;
  accepted.write(encode({ channel: 2, kind: KIND.event, payload: {
    slot: 'storybook-main', event: 'Invoke', id: `${inspect}:Invoke`, node: inspect,
  } }));
  const inspection = await until(
    () => calls.filter((call) => call.call === 'interface_render_at').slice(renderCount)
      .find((call) => call.with.frame.patches.some((patch) => patch.Create?.tag === 'LogView')),
    `container inspection never crossed the socket; stderr=${stderr}`,
  );
  assert.ok(inspection.with.frame.patches.some((patch) => patch.Create?.tag === 'LogView'));
  await until(
    () => calls.filter((call) => call.call === 'interface_render_at').slice(renderCount)
      .find((call) => call.with.frame.patches.some((patch) => patch.SetProp?.value?.Text === 'Loaded 2 bounded processes for api.')),
    `container inspection status never crossed the socket; stderr=${stderr}`,
  );
  assert.equal(stderr, '');
});

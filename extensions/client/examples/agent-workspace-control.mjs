#!/usr/bin/env node
import { connect, workspace } from '@husklet/client';

const input = JSON.parse(process.argv[2] ?? 'null');
if (!input || typeof input.path !== 'string' || typeof input.name !== 'string'
  || typeof input.image !== 'string' || !['amd64', 'arm64'].includes(input.architecture)) {
  throw new TypeError('usage: agent-workspace-control.mjs JSON(path, name, image, architecture)');
}

const configuration = {
  name: input.name, image: input.image, architecture: input.architecture,
  storage: null, shell: null, cpus: null, memory_mb: null, environment: [], mounts: [],
  docker_socket: false, scrollback: null, vpn: null, execution_lifetime: 'persisted',
  terminal: { font_family: null, font_size: null, foreground: null, background: null, cursor_shape: null, cursor_blink: null },
};
const session = await connect({ path: input.path });
const host = workspace(session); let lastRevision = -1; let created; let running = false; let removed = false;
const mutate = async (action, authority) => {
  let observed;
  const event = new Promise((resolve) => { observed = (change) => {
    if (change.workspace === input.name && change.action === action && change.revision > lastRevision) resolve(change);
  }; });
  const stop = await host.watchWorkspaceLifecycle(observed);
  let timer;
  try {
    const result = await authority();
    const change = await Promise.race([event, new Promise((_, reject) => { timer = setTimeout(() => reject(new Error(`${action} lifecycle event timed out`)), 1_000); })]);
    lastRevision = change.revision;
    return result;
  } finally {
    clearTimeout(timer); await stop();
  }
};
try {
  created = await mutate('create', () => host.create(configuration));
  await mutate('start', () => host.start(input.name));
  running = true;
  await mutate('stop', () => host.stop(input.name));
  running = false;
  await mutate('remove', () => host.delete(input.name, created.generation));
  removed = true;
  process.stdout.write(`${JSON.stringify({ name: created.name, generation: created.generation, revision: lastRevision })}\n`);
} finally {
  try {
    if (created && !removed) {
      if (running) await host.stop(input.name).catch(() => {});
      await host.delete(input.name, created.generation).catch(() => {});
    }
  } finally {
    await session.close();
  }
}

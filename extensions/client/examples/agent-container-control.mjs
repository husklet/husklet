#!/usr/bin/env node
import { connect, workspace } from '@husklet/client';

const configuration = JSON.parse(process.argv[2] ?? 'null');
if (!configuration || typeof configuration.path !== 'string'
  || typeof configuration.containerId !== 'string' || !Array.isArray(configuration.command)) {
  throw new TypeError('usage: agent-container-control.mjs JSON(path, containerId, command[])');
}

const session = await connect({ path: configuration.path });
let started = false;
try {
  const host = workspace(session);
  const workspaceInfo = await host.info();
  const container = await host.containers.inspect(configuration.containerId);
  if (container.state !== 'running') {
    const result = await host.containers.startAndWait(container.id, { timeoutMs: 1_000 });
    if (!result.changed) throw new Error(`container ${container.id} did not become observably running`);
    started = true;
  }
  const { execution, output } = await host.containers.execAndWait(container.id, {
    command: configuration.command, timeoutMs: 1_000, stdout: true, stderr: true,
  });
  await host.containers.removeExecution(execution.id);
  process.stdout.write(`${JSON.stringify({ workspace: workspaceInfo.name, container: container.id, execution, output })}\n`);
} finally {
  try {
    if (started) {
      const host = workspace(session);
      const result = await host.containers.stopAndWait(configuration.containerId, { timeoutMs: 1_000 });
      if (!result.changed) throw new Error(`container ${configuration.containerId} did not return to its initial stopped state`);
    }
  } finally {
    await session.close();
  }
}

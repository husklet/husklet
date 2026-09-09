#!/usr/bin/env node
import { connect, workspace } from '@husklet/client';

const configuration = JSON.parse(process.argv[2] ?? 'null');
if (
  !configuration ||
  typeof configuration.path !== 'string' ||
  typeof configuration.containerId !== 'string' ||
  !Array.isArray(configuration.command) ||
  (configuration.input !== undefined && typeof configuration.input !== 'string')
) {
  throw new TypeError(
    'usage: agent-container-control.mjs JSON(path, containerId, command[], input?)',
  );
}

const session = await connect({ path: configuration.path });
let started = false;
let generation;
try {
  const host = workspace(session);
  const workspaceInfo = await host.info();
  const container = await host.containers.inspect(configuration.containerId);
  generation = container.generation;
  if (container.state !== 'running') {
    const result = await host.containers.startAndWait(container.id, generation, {
      timeoutMs: 1_000,
    });
    if (!result.changed)
      throw new Error(`container ${container.id} did not become observably running`);
    started = true;
    generation = result.container.generation;
  }
  const executionId = await host.containers.exec(container.id, generation, {
    command: configuration.command,
    stdin: true,
  });
  // A terminal/LLM bridge can yield many bounded token or prompt chunks here.
  // EOF is deliberate: losing the extension socket must not truncate a prompt.
  if (configuration.input !== undefined) {
    await host.containers.pipeExecutionStdin(executionId, [configuration.input]);
  } else {
    await host.containers.closeExecutionStdin(executionId);
  }
  const execution = await host.containers.waitExecution(executionId, { timeoutMs: 1_000 });
  const output = await host.containers.executionLogs(executionId, { stdout: true, stderr: true });
  await host.containers.removeExecution(execution.id);
  process.stdout.write(
    `${JSON.stringify({ workspace: workspaceInfo.name, container: container.id, execution, output })}\n`,
  );
} finally {
  try {
    if (started) {
      const host = workspace(session);
      const result = await host.containers.stopAndWait(configuration.containerId, generation, {
        timeoutMs: 1_000,
      });
      if (!result.changed)
        throw new Error(
          `container ${configuration.containerId} did not return to its initial stopped state`,
        );
    }
  } finally {
    await session.close();
  }
}

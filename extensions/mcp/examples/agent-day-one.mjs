const text = (answer, tool) => {
  const value = answer?.content?.find(({ type }) => type === 'text')?.text;
  if (answer?.isError || typeof value !== 'string') throw new Error(`${tool} failed: ${value ?? 'no text result'}`);
  return value;
};
const call = async (client, name, args = {}) => text(await client.callTool({ name, arguments: args }), name);
const json = async (client, name, args) => JSON.parse(await call(client, name, args));
const xmlNumber = (xml, name) => {
  const match = xml.match(new RegExp(`(?:<pane|<node)[^>]*\\b${name}="(\\d+)"`));
  if (!match) throw new Error(`semantic snapshot has no numeric ${name}`);
  return Number(match[1]);
};
const nodeForLabel = (xml, label) => {
  const encoded = label.replaceAll('&', '&amp;').replaceAll('<', '&lt;').replaceAll('>', '&gt;')
    .replaceAll('"', '&quot;').replaceAll("'", '&apos;');
  const escaped = encoded.replace(/[.*+?^${}()|[\]\\]/g, '\\$&');
  const match = xml.match(new RegExp(`<node\\s+id="(\\d+)"[^>]*>\\s*<label>${escaped}</label>`));
  if (!match) throw new Error(`semantic snapshot has no ${JSON.stringify(label)} action`);
  return Number(match[1]);
};

/** Acquire one image through bounded event waits; never poll the status endpoint. */
export async function pullImageForAgent(client, reference, waitMs = 30_000) {
  const { job } = await json(client, 'husklet_image_pull_start', { reference });
  let status = await json(client, 'husklet_image_pull_status', { job });
  for (let delivered = 0; delivered < 128 && !['complete', 'failed', 'cancelled'].includes(status.state); delivered += 1) {
    const update = await json(client, 'husklet_image_pull_wait', { job, after_revision: status.revision, timeout_ms: waitMs });
    if (!update.changed) throw new Error(`image pull ${job} made no progress before its bounded wait expired`);
    status = update.status;
  }
  if (status.state !== 'complete') throw new Error(`image pull ${job} ended as ${status.state}: ${status.error ?? 'no detail'}`);
  return status;
}

/**
 * A bounded first-day workflow using only strict Husklet MCP tools.
 *
 * The target workspace must be a workspace the observer is allowed to control,
 * not necessarily the workspace hosting the observer socket. Configuration and
 * containers created by this turn are restored/removed in `finally`.
 */
export async function runAgentDayOne(client, {
  workspaceName,
  updatedConfiguration,
  container: { image, name: containerName, command },
  terminalInput = 'help\n',
  actionLabel = 'Refresh',
  waitMs = 5_000,
  pullImage = false,
}) {
  if (!Array.isArray(command) || command.length === 0) throw new TypeError('container.command must be a non-empty argv array');
  if (typeof terminalInput !== 'string') throw new TypeError('terminalInput must be literal text');
  const original = await json(client, 'husklet_workspace_inspect', { name: workspaceName });
  let configurationChanged = false;
  let container;
  const cleanupErrors = [];
  try {
    const imagePull = pullImage ? await pullImageForAgent(client, image, waitMs) : null;
    await call(client, 'husklet_workspace_update', {
      name: workspaceName, generation: original.generation, configuration: updatedConfiguration, confirm: true,
    });
    configurationChanged = true;
    container = await json(client, 'husklet_container_create', {
      image, name: containerName, command,
      labels: [['husklet.agent-workflow', 'day-one']],
      memory_mb: 512, cpus: 1, pids_limit: 128,
    });
    await call(client, 'husklet_container_start', { id: container.id });
    const execution = await json(client, 'husklet_container_exec', { id: container.id, command });
    const processes = await json(client, 'husklet_container_processes', { id: container.id });

    const inventory = await json(client, 'husklet_pane_list');
    const terminal = inventory.panes?.find(({ kind }) => kind === 'terminal');
    const semantic = inventory.panes?.find(({ kind }) => kind === 'surface' || kind === 'native');
    if (!terminal || !semantic) throw new Error('one terminal and one semantic pane are required');
    const terminalBefore = await call(client, 'husklet_pane_read', { slot: terminal.slot, lines: 100 });
    const terminalWaiting = json(client, 'husklet_pane_wait', { slot: terminal.slot, timeout_ms: waitMs });
    await call(client, 'husklet_terminal_write', { slot: terminal.slot, input: terminalInput });
    const terminalChanged = await terminalWaiting;

    const semanticBefore = await call(client, 'husklet_pane_snapshot', { slot: semantic.slot });
    const revision = xmlNumber(semanticBefore, 'revision');
    const node = nodeForLabel(semanticBefore, actionLabel);
    const semanticWaiting = json(client, 'husklet_pane_wait', { slot: semantic.slot, timeout_ms: waitMs });
    await call(client, 'husklet_pane_action', { slot: semantic.slot, revision, node, action: 'invoke' });
    const semanticChanged = await semanticWaiting;
    const semanticAfter = semanticChanged.changed
      ? await call(client, 'husklet_pane_snapshot', { slot: semantic.slot }) : null;
    return {
      workspace: { before: original, applied: updatedConfiguration },
      container: { imagePull, created: container, execution, processes },
      terminal: { slot: terminal.slot, before: terminalBefore, changed: terminalChanged },
      semantic: { slot: semantic.slot, revision, node, before: semanticBefore, changed: semanticChanged, after: semanticAfter },
    };
  } finally {
    if (container?.id) {
      try { await call(client, 'husklet_container_stop', { id: container.id, confirm: true }); } catch (error) { cleanupErrors.push(error); }
      try { await call(client, 'husklet_container_remove', { id: container.id, confirm: true }); } catch (error) { cleanupErrors.push(error); }
    }
    if (configurationChanged) {
      const { generation: _, ...originalConfiguration } = original;
      try { await call(client, 'husklet_workspace_update', { name: workspaceName, generation: original.generation, configuration: originalConfiguration, confirm: true }); }
      catch (error) { cleanupErrors.push(error); }
    }
    if (cleanupErrors.length > 0) throw new AggregateError(cleanupErrors, 'day-one cleanup failed');
  }
}

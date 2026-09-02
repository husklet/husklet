const text = (answer, tool) => {
  const value = answer?.content?.find(({ type }) => type === 'text')?.text;
  if (answer?.isError || typeof value !== 'string') throw new Error(`${tool} failed: ${value ?? 'no text result'}`);
  return value;
};
const call = async (client, name, args = {}) => text(await client.callTool({ name, arguments: args }), name);
const json = async (client, name, args) => JSON.parse(await call(client, name, args));

/**
 * Create and exercise a workspace while confining file and pane operations to
 * the workspace named by the socket credential. Every created resource is
 * removed in reverse order, including after a failed intermediate operation.
 */
export async function runAgentAdmin(client, {
  hostingWorkspace,
  workspaceConfiguration,
  directory,
  file,
  contents,
  eventSlot,
  eventInput = '',
  waitMs = 5_000,
}) {
  const hosting = await json(client, 'husklet_workspace_info');
  if (hosting?.name !== hostingWorkspace) {
    throw new Error(`filesystem workspace ${JSON.stringify(hostingWorkspace)} does not match socket workspace ${JSON.stringify(hosting?.name ?? null)}`);
  }
  if (workspaceConfiguration.name === hostingWorkspace) {
    throw new Error('managed workspace must be distinct from the socket workspace');
  }
  let workspaceCreated = false;
  let workspaceStarted = false;
  let directoryCreated = false;
  let fileCreated = false;
  const cleanupErrors = [];
  try {
    const created = await json(client, 'husklet_workspace_create', { configuration: workspaceConfiguration });
    workspaceCreated = true;
    await call(client, 'husklet_workspace_start', { name: workspaceConfiguration.name });
    workspaceStarted = true;
    await call(client, 'husklet_file_mkdir', { path: directory });
    directoryCreated = true;
    await call(client, 'husklet_file_write', { path: file, contents });
    fileCreated = true;
    const read = await json(client, 'husklet_file_read', { path: file });

    const waiting = json(client, 'husklet_pane_wait', { slot: eventSlot, timeout_ms: waitMs });
    await call(client, 'husklet_terminal_write', { slot: eventSlot, input: eventInput });
    const event = await waiting;
    return { hosting, created, read, event };
  } finally {
    if (fileCreated) {
      try { await call(client, 'husklet_file_remove', { path: file, confirm: true }); } catch (error) { cleanupErrors.push(error); }
    }
    if (directoryCreated) {
      try { await call(client, 'husklet_file_remove', { path: directory, confirm: true }); } catch (error) { cleanupErrors.push(error); }
    }
    if (workspaceStarted) {
      try { await call(client, 'husklet_workspace_stop', { name: workspaceConfiguration.name }); } catch (error) { cleanupErrors.push(error); }
    }
    if (workspaceCreated) {
      try { await call(client, 'husklet_workspace_delete', { name: workspaceConfiguration.name, confirm: true }); }
      catch (error) { cleanupErrors.push(error); }
    }
    if (cleanupErrors.length > 0) throw new AggregateError(cleanupErrors, 'administrative cleanup failed');
  }
}

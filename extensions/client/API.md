# @husklet/client API reference

This reference is generated from the public `protocolSurface`, which is itself
closed over the authoritative Rust protocol schema. A stale operation, topic, or
capability makes `npm test` fail; regenerate intentionally with `npm run api:generate`.

Create one typed facade and reuse it:

```js
import { connect, workspace } from '@husklet/client';
const session = await connect({ timeout: 10_000, pendingLimit: 32 });
const host = workspace(session);
const panes = await host.terminal.panes();
const readable = await host.terminal.toText(panes.panes[0].slot, { lines: 200 });
console.log(readable.text);
const next = await host.terminal.waitForText(panes.panes[0].slot, readable.snapshot);
if (next.changed) console.log(next.readable.text);
await session.close();
```

Every call is checked against the capabilities granted by the handshake. A denied,
absent, conflicting, failed, or unsupported host reply rejects with `ExtensionError`;
branch on `error.kind`, not message text. Pending calls are bounded and time out by
closing the ordered session, because continuing could attach a later reply to the
wrong caller.

## Workspace

- `host.info(...)` — `workspace_info`, requires `workspace-read`.
- `host.list(...)` — `workspace_list`, requires `workspace-read`.
- `host.inspect(...)` — `workspace_inspect`, requires `workspace-read`.
- `host.create(...)` — `workspace_create`, requires `workspace-control`.
- `host.adopt(...)` — `workspace_adopt`, requires `workspace-control`.
- `host.update(...)` — `workspace_update`, requires `workspace-control`.
- `host.delete(...)` — `workspace_delete`, requires `workspace-control`.
- `host.start(...)` — `workspace_start`, requires `workspace-control`.
- `host.stop(...)` — `workspace_stop`, requires `workspace-control`.
- `host.restart(...)` — `workspace_restart`, requires `workspace-control`.

## Containers

- `host.containers.list(...)` — `container_list`, requires `container-read`.
- `host.containers.inspect(...)` — `container_inspect`, requires `container-read`.
- `host.containers.logs(...)` — `container_logs`, requires `container-read`.
- `host.containers.create(...)` — `container_create`, requires `container-control`.
- `host.containers.start(...)` — `container_start`, requires `container-control`.
- `host.containers.stop(...)` — `container_stop`, requires `container-control`.
- `host.containers.remove(...)` — `container_remove`, requires `container-control`.
- `host.containers.pause(...)` — `container_pause`, requires `container-control`.
- `host.containers.unpause(...)` — `container_unpause`, requires `container-control`.
- `host.containers.restart(...)` — `container_restart`, requires `container-control`.
- `host.containers.rename(...)` — `container_rename`, requires `container-control`.
- `host.containers.kill(...)` — `container_kill`, requires `container-control`.
- `host.containers.exec(...)` — `container_exec`, requires `container-control`.
- `host.containers.attachTerminal(...)` — `container_attach_terminal`, requires `container-attach`.

## Processes and executions

- `host.containers.processes(...)` — `container_processes`, requires `container-read`.
- `host.containers.execution(...)` — `execution_inspect`, requires `container-read`.
- `host.containers.executions(...)` — `execution_list`, requires `container-read`.
- `host.containers.executionLogs(...)` — `execution_logs`, requires `container-read`.
- `host.containers.waitExecution(...)` — `execution_wait`, requires `container-read`.
- `host.containers.signalExecution(...)` — `execution_kill`, requires `container-control`.
- `host.containers.removeExecution(...)` — `execution_remove`, requires `container-control`.
- `host.containers.execAndWait(id, options)` — prevalidates bounded execution/output options, executes by immutable container ID, waits, then fetches bounded logs; failures retain the execution ID in `ExecutionOperationError` and never auto-remove the record.

## Terminal and panes

- `host.terminal.tabs(...)` — `terminal_tabs`, requires `terminal-read`.
- `host.terminal.topology(...)` — `terminal_topology`, requires `terminal-read`.
- `host.terminal.panes(...)` — `pane_list`, requires `pane-observe`.
- `host.terminal.openTab(...)` — `terminal_open_tab`, requires `terminal-control`.
- `host.terminal.split(...)` — `terminal_split`, requires `terminal-control`.
- `host.terminal.splitObserved(...)` — `terminal_split_observed`, requires `terminal-control`.
- `host.terminal.spawn(...)` — `terminal_spawn`, requires `terminal-control`.
- `host.terminal.spawnObserved(...)` — `terminal_spawn_observed`, requires `terminal-control`.
- `host.terminal.read(...)` — `terminal_read_pane`, requires `terminal-output`.
- `host.terminal.writeInput(...)` — `terminal_write_pane`, requires `terminal-control`.
- `host.terminal.resizeGrid(...)` — `terminal_resize_grid`, requires `terminal-control`.
- `host.terminal.resizeGridObserved(...)` — `terminal_resize_grid_observed`, requires `terminal-control`.
- `host.terminal.close(...)` — `terminal_close_pane`, requires `terminal-control`.
- `host.terminal.closeObserved(...)` — `terminal_close_pane_observed`, requires `terminal-control`.
- `host.terminal.focus(...)` — `terminal_focus_pane`, requires `terminal-control`.
- `host.terminal.focusObserved(...)` — `terminal_focus_pane_observed`, requires `terminal-control`.
- `host.terminal.retitle(...)` — `terminal_retitle_pane`, requires `terminal-control`.
- `host.terminal.retitleObserved(...)` — `terminal_retitle_pane_observed`, requires `terminal-control`.
- `host.terminal.ratio(...)` — `terminal_ratio`, requires `terminal-control`.
- `host.terminal.ratioObserved(...)` — `terminal_ratio_observed`, requires `terminal-control`.
- `host.terminal.switchOccupant(...)` — `terminal_switch_occupant`, requires `terminal-control`.
- `host.terminal.switchOccupantObserved(...)` — `terminal_switch_occupant_observed`, requires `terminal-control`.
- `host.terminal.toText(...)` — discovers a pane and returns visible terminal screen text or bounded semantic XML; requires `pane-observe` and the corresponding `terminal-output` or `pane-semantic-read` grant.
- `host.terminal.waitForText(...)` — arms pane-change observation, ignores the unchanged cursor, then returns a fresh bounded text projection; requires `pane-observe` and the corresponding read grant.
- `host.terminal.actAndWait(...)` — arms pane observation before a revision-bound semantic action, then returns its changed bounded projection; requires `pane-observe`, `pane-semantic-control`, and the corresponding read grant.
- `host.terminal.switchOccupantAndWait(...)` — arms observation before an observed occupant switch and verifies the exact terminal or extension/provider identity; requires `pane-observe` and `terminal-control`.
- `host.terminal.splitAndWait(...)` — arms pane changes before a generation/revision-bound split and verifies the returned child slot from bounded inventory; requires `pane-observe` and `terminal-control`.
- `host.terminal.closeAndWait(...)` — arms pane changes before a generation/revision-bound close and proves absence only from a complete pane inventory; requires `pane-observe` and `terminal-control`.

## Files

- `host.files.list(...)` — `filesystem_list`, requires `filesystem-read`.
- `host.files.read(...)` — `filesystem_read`, requires `filesystem-read`.
- `host.files.readRange(...)` — `filesystem_read_range`, requires `filesystem-read`.
- `host.files.stat(...)` — `filesystem_stat`, requires `filesystem-read`.
- `host.files.write(...)` — `filesystem_write`, requires `filesystem-write`.
- `host.files.createObserved(...)` — `filesystem_create_observed`, requires `filesystem-write`.
- `host.files.mkdir(...)` — `filesystem_mkdir`, requires `filesystem-write`.
- `host.files.rename(...)` — `filesystem_rename`, requires `filesystem-write`.
- `host.files.renameObserved(...)` — `filesystem_rename_observed`, requires `filesystem-write`.
- `host.files.remove(...)` — `filesystem_remove`, requires `filesystem-write`.
- `host.files.removeObserved(...)` — `filesystem_remove_observed`, requires `filesystem-write`.

## Images

- `host.images.list(...)` — `image_list`, requires `image-read`.
- `host.images.pull(...)` — `image_pull`, requires `image-write`.
- `host.images.startPull(...)` — `image_pull_start`, requires `image-write`.
- `host.images.pullStatus(...)` — `image_pull_status`, requires `image-write`.
- `host.images.cancelPull(...)` — `image_pull_cancel`, requires `image-write`.
- `host.images.inspect(...)` — `image_inspect`, requires `image-read`.
- `host.images.remove(...)` — `image_remove`, requires `image-write`.
- `host.images.prune(...)` — `image_prune`, requires `image-write`.

## Networks

- `host.networks.list(...)` — `network_list`, requires `network-read`.
- `host.networks.inspect(...)` — `network_inspect`, requires `network-read`.
- `host.networks.create(...)` — `network_create`, requires `network-write`.
- `host.networks.remove(...)` — `network_remove`, requires `network-write`.
- `host.networks.connect(...)` — `network_connect`, requires `network-write`.
- `host.networks.disconnect(...)` — `network_disconnect`, requires `network-write`.

## Volumes

- `host.volumes.list(...)` — `volume_list`, requires `volume-read`.
- `host.volumes.inspect(...)` — `volume_inspect`, requires `volume-read`.
- `host.volumes.create(...)` — `volume_create`, requires `volume-write`.
- `host.volumes.remove(...)` — `volume_remove`, requires `volume-write`.

## Extensions

- `host.extensions.list(...)` — `extension_list`, requires `extension-read`.
- `host.extensions.inspect(...)` — `extension_inspect`, requires `extension-read`.
- `host.extensions.enable(...)` — `extension_enable`, requires `extension-control`.
- `host.extensions.disable(...)` — `extension_disable`, requires `extension-control`.
- `host.extensions.retry(...)` — `extension_retry`, requires `extension-control`.
- `host.extensions.remove(...)` — `extension_remove`, requires `extension-control`.
- `host.extensions.startAcquisition(...)` — `extension_acquisition_start`, requires `extension-install`.
- `host.extensions.acquisition(...)` — `extension_acquisition_status`, requires `extension-install`.
- `host.extensions.cancelAcquisition(...)` — `extension_acquisition_cancel`, requires `extension-install`.
- `host.extensions.install(...)` — `extension_install`, requires `extension-install`.
- `host.extensions.update(...)` — `extension_update`, requires `extension-install`.
- `host.extensions.waitForAcquisition(...)` — waits for an exact acquisition job revision to advance, then reads its authoritative full status; requires `extension-install`.
- `host.extensions.enableAndWait(...)` — arms inventory before enabling an exact installed digest, then verifies its durable enabled state; requires `extension-read` and `extension-control`.
- `host.extensions.disableAndWait(...)` — arms inventory before disabling an exact installed digest, then verifies durable standby; provider withdrawal remains separately observable; requires `extension-read` and `extension-control`.
- `host.extensions.retryAndWait(...)` — arms inventory before retrying an exact faulted digest, rejects replacement/disappearance, then verifies durable duty; requires `extension-read` and `extension-control`.
- `host.extensions.removeAndWait(...)` — arms inventory before removing an exact installed digest, then proves that digest is absent and reports any same-name replacement; requires `extension-read` and `extension-control`.
- `host.extensions.installAndWait(...)` / `updateAndWait(...)` — inspect the exact ready acquisition revision, arm inventory before commit, and verify the returned and published name/digest; requires `extension-install` and `extension-read`.
- `host.containers.startAndWait(...)` — acknowledges bounded inventory before starting an immutable ID, ignores the unchanged initial snapshot, and returns only on a later running state; requires `container-read` and `container-control`.
- `host.containers.stopAndWait(...)` — acknowledges bounded inventory before stopping an immutable ID, ignores unchanged/running snapshots, and returns only on a later exited state; requires `container-read` and `container-control`.
- `host.containers.removeAndWait(...)` — arms an explicit completeness-bearing inventory before removal and accepts absence only from a later `complete: true` snapshot; requires `container-read` and `container-control`.
- `host.containers.restartAndWait(...)` — arms inventory before restarting an immutable ID and accepts only `running` at a generation newer than the caller observed; requires `container-read` and `container-control`.

## Semantics

- `host.terminal.semantics(...)` — `pane_semantic_read`, requires `pane-semantic-read`.
- `host.terminal.act(...)` — `pane_semantic_action`, requires `pane-semantic-control`.

## Observe before mutating

Inventory, inspection, pane text, file ranges, executions, pulls, and acquisitions
return the identity/generation/revision fields accepted by their observed or
destructive counterparts. Keep those exact values through user or agent consent;
do not replace them with names, prefixes, mutable tags, PIDs, or a newer snapshot.
Legacy unobserved pane/file methods remain for compatibility, while the `Observed`
methods are the safe default. Process PIDs are snapshot display values and may be reused.

```js
const pane = (await host.terminal.panes()).panes[0];
const tree = await host.terminal.semantics(pane.slot);
await host.terminal.act(pane.slot, {
  generation: tree.generation, revision: tree.revision,
  node: tree.root.id, action: 'focus',
});
```

## Events

Subscriptions are credit-controlled and bounded. The host sends an initial snapshot,
coalesces latest state while credit is exhausted, and returns credit only after the
client delivers an event. Always unsubscribe or use a `watch*` disposer.

- `host.subscribe('containers')` / `host.unsubscribe('containers')` — requires `container-read`.
- `host.subscribe('container-inventory')` / `host.unsubscribe('container-inventory')` — requires `container-read`.
- `host.subscribe('executions')` / `host.unsubscribe('executions')` — requires `container-read`.
- `host.subscribe('images')` / `host.unsubscribe('images')` — requires `image-read`.
- `host.subscribe('image-pulls')` / `host.unsubscribe('image-pulls')` — requires `image-write`.
- `host.subscribe('volumes')` / `host.unsubscribe('volumes')` — requires `volume-read`.
- `host.subscribe('networks')` / `host.unsubscribe('networks')` — requires `network-read`.
- `host.subscribe('terminal')` / `host.unsubscribe('terminal')` — requires `terminal-read`.
- `host.subscribe('pane-changes')` / `host.unsubscribe('pane-changes')` — requires `pane-observe`.
- `host.subscribe('extensions')` / `host.unsubscribe('extensions')` — requires `extension-read`.
- `host.subscribe('extension-acquisitions')` / `host.unsubscribe('extension-acquisitions')` — requires `extension-install`.
- `host.subscribe('workspace-lifecycle')` / `host.unsubscribe('workspace-lifecycle')` — requires `workspace-read`.
- `host.subscribe('workspace-events')` / `host.unsubscribe('workspace-events')` — requires `workspace-events`.

## Protocol bounds

The generated `PROTOCOL_BOUNDS` values are:

- `extension_job_bytes`: 128
- `extension_reference_bytes`: 512
- `pane_input_bytes`: 65536
- `pane_inventory_items`: 512
- `pane_text_bytes`: 524288
- `semantic_action_value_bytes`: 4096
- `semantic_depth`: 32
- `semantic_nodes`: 256
- `semantic_text_bytes`: 256
- `terminal_command_argument_bytes`: 4096
- `terminal_command_bytes`: 32768

Collection replies also carry their own `truncated`/`eof` fields where defined.
Terminal reads are interpreted bounded screen/history snapshots—not raw stdout/stderr.
Container and execution log methods return bounded stdout/stderr byte arrays with
completeness flags. Semantic XML escapes values, redacts sensitive fields, and applies
depth, node, and text bounds.

## Renderer-internal requests

These are intentionally owned by `@husklet/react` rather than exposed as ordinary
workspace facade calls:

- `interface_open_tab` — owned by the React/native renderer root lifecycle.
- `interface_split` — owned by the React/native renderer root lifecycle.
- `interface_withdraw` — owned by the React/native renderer root lifecycle.
- `interface_render` — owned by the React/native renderer commit transport.
- `interface_render_at` — owned by the React/native renderer commit transport.
- `source_resize` — owned by the React/native renderer virtual source transport.
- `source_resize_at` — owned by the React/native renderer virtual source transport.

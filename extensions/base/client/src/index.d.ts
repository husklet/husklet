import type { WireCall, WireReplyFor, WireRequestFor, WireUiEvent } from './generated-protocol.js';

export type Topic = 'containers' | 'container-inventory' | 'images' | 'volumes' | 'networks' | 'terminal' | 'pane-changes' | 'executions' | 'image-pulls' | 'extensions' | 'extension-acquisitions' | 'workspace-lifecycle' | 'workspace-events';
export type Division = 'beside' | 'below';
export interface WorkspaceInfo { name: string; architecture: string; image: string }
export interface ExtensionPaneProvider { id: string; title: string; icon: string | null }
export interface ExtensionSummary { name: string; image_digest: string; status: string; version?: string; enabled?: boolean; pane_providers?: ExtensionPaneProvider[] }
export interface ExtensionProviderDeclaration { extension: string; image_digest: string; version: string; status: string; id: string; title: string; icon: string | null }
export interface ExtensionProviderCatalogue { providers: ExtensionProviderDeclaration[]; truncated: boolean }
export type ExtensionCapability =
  | 'workspaces:read' | 'workspaces:control' | 'workspaces:events'
  | 'containers:read' | 'containers:control' | 'containers:attach' | 'images:read' | 'images:write'
  | 'volumes:read' | 'volumes:write' | 'networks:read' | 'networks:write'
  | 'terminals:read' | 'terminals:control' | 'terminals:output' | 'panes:observe'
  | 'panes:semantic-read' | 'panes:semantic-control' | 'extensions:read'
  | 'extensions:control' | 'extensions:install' | 'filesystem:read'
  | 'filesystem:write' | 'interface:render';
export interface ExtensionCandidate { name: string; version: string; image_digest: string; requested: ExtensionCapability[]; installed_image_digest: string | null }
export interface ExtensionAcquisitionJob { job: string }
export interface ExtensionAcquisitionProgress { status: string; id: string | null; current: number | null; total: number | null }
export interface ExtensionAcquisitionStatus { job: string; reference: string; revision: number; state: string; progress: ExtensionAcquisitionProgress | null; candidate: ExtensionCandidate | null; error: string | null }
export interface ExtensionAcquisitionChange { job: string; revision: number; state: string; coalesced: number }
export interface WorkspaceState extends WorkspaceInfo { running: boolean; current: boolean }
export interface WorkspaceMount { host: string; container: string; read_only: boolean }
export interface WorkspaceTerminal {
  font_family: string | null;
  font_size: number | null;
  foreground: string | null;
  background: string | null;
  cursor_shape: string | null;
  cursor_blink: boolean | null;
}
export interface WorkspaceConfiguration extends WorkspaceInfo {
  generation?: string;
  storage: string | null;
  shell: string | null;
  cpus: number | null;
  memory_mb: number | null;
  environment: [string, string][];
  mounts: WorkspaceMount[];
  docker_socket: boolean;
  scrollback: number | null;
  vpn: string | null;
  execution_lifetime: 'persisted' | 'live' | 'ephemeral';
  terminal: WorkspaceTerminal;
}
export interface ContainerSummary { id: string; name: string; image: string; state: string; created: number; generation?: number }
export interface ContainerInventory { containers: ContainerSummary[]; complete: boolean }
export interface ContainerVolumeMount { volume: string; target: string; read_only?: boolean }
export interface ContainerPort { container: number; host?: number | null; protocol: 'tcp' | 'udp' }
export interface ContainerCreateSpec {
  image: string; name: string; hostname?: string | null; entrypoint?: string[] | null; command?: string[];
  environment?: [string, string][]; working_directory?: string | null; user?: string | null;
  labels?: [string, string][]; mounts?: ContainerVolumeMount[]; network?: string | null;
  ports?: ContainerPort[]; memory_mb?: number | null; cpus?: number | null; pids_limit?: number | null;
}
export interface ProcessList {
  container_id: string; titles: string[]; processes: string[][]; observed_at_ms: number;
  scope: 'initial' | 'namespace'; pid_identity: 'snapshot'; truncated: boolean;
}
export interface ContainerOutput {
  stdout: number[]; stderr: number[]; truncated: boolean;
  stdout_truncated: boolean; stderr_truncated: boolean; eof: boolean;
}
export interface ExecutionSummary {
  id: string; container_id: string; running: boolean; exit_code: number; pid: number;
  command: string[]; user: string;
}
export interface ExecutionList { executions: ExecutionSummary[]; truncated: boolean }
export interface ImageSummary { id: string; reference: string; size: number; created: number }
export interface ImageInventory { images: ImageSummary[]; truncated: boolean }
export interface ImageDetails { id: string; references: string[]; created: string; size: number; os: string; architecture: string; entrypoint: string[]; command: string[]; working_directory: string; user: string }
export interface ImagePruneResult { deleted: number; space_reclaimed: number }
export interface ImagePullJob { job: string }
export interface ImagePullStatus { job: string; reference: string; revision: number; state: string; status: string | null; layer: string | null; current: number | null; total: number | null; image: ImageSummary | null; error: string | null }
export interface ImagePullChange { job: string; revision: number; state: string; coalesced: number }
export interface VolumeSummary { name: string; driver: string; generation: string }
export interface VolumeInventory { volumes: VolumeSummary[]; truncated: boolean }
export interface NetworkSummary { id: string; name: string; driver: string; scope: string }
export interface NetworkInventory { networks: NetworkSummary[]; truncated: boolean }
export interface PaneSummary {
  slot: string;
  working_directory: string | null;
  command: string | null;
  occupant: 'terminal' | 'surface';
  provider: { extension: string; provider: string } | null;
}
export interface TabSummary { id: string; title: string; pinned: boolean; panes: PaneSummary[] }
export interface PaneText { slot: string; generation?: number; revision?: number; columns?: number; rows?: number; lines: string[]; cursor_column?: number; cursor_row?: number; truncated: boolean }
export interface PaneChange { slot: string; kind: 'terminal' | 'surface' | 'native'; revision: number; generation: number; coalesced: number }
export interface InspectablePane { slot: string; generation: number; revision: number; kind: 'terminal' | 'surface' | 'native'; provider: { extension: string; provider: string } | null; tab: string | null; title: string | null; focused: boolean }
export interface PaneInventory { panes: InspectablePane[]; truncated: boolean }
export type SemanticActionKind = 'invoke' | 'change' | 'submit' | 'toggle' | 'expand' | 'focus';
export interface SemanticNode { id: number; role: string; label: string | null; value: string | null; disabled: boolean; destructive: boolean; actions: SemanticActionKind[]; children: SemanticNode[] }
export interface PaneSemanticTree { slot: string; generation: number; revision: number; root: SemanticNode; truncated: boolean }
export type ReadablePane =
  | { kind: 'terminal'; text: string; snapshot: PaneText }
  | { kind: 'ui'; text: string; snapshot: PaneSemanticTree };
export interface PaneSemanticAction { generation: number; revision: number; node: number; action: SemanticActionKind; value?: string | null }
export interface GridSize { columns: number; rows: number }
export type LayoutNode =
  | { kind: 'pane'; pane: PaneSummary; grid: GridSize | null; focused: boolean }
  | { kind: 'split'; division: Division; ratio_per_mille: number; first: LayoutNode; second: LayoutNode };
export interface TabTopology { id: string; title: string; pinned: boolean; root: LayoutNode }
export interface TerminalTopology { active_tab: string | null; tabs: TabTopology[] }
export interface FileEntry { path: string; directory: boolean; size: number; identity?: string | null }
export interface FileRange { path: string; identity: string; offset: number; total: number; contents: number[]; eof: boolean; truncated: boolean }
export type WorkspaceEvent =
  | { event: 'key'; key: string; modifiers: string[]; pressed: boolean; slot?: string | null; generation?: number | null }
  | { event: 'focus'; active: boolean; slot?: string | null; generation?: number | null }
  | { event: 'pointer'; phase: 'move' | 'enter' | 'leave' | 'press' | 'release' | 'click' | 'context' | 'scroll'; slot: string; generation: number; x: number; y: number; button: number | null; modifiers: string[]; delta_x: number | null; delta_y: number | null };
export interface WorkspaceEventBatch { events: WorkspaceEvent[]; dropped: number }
export interface WorkspaceLifecycleChange { workspace: string; action: 'create' | 'update' | 'remove' | 'start' | 'stop' | 'restart'; revision: number; coalesced: number }
export interface PaneSelection { pane_provider: string; slot: string }
export interface InterfaceEventBase<I extends string, T extends string> {
  interaction: I; trigger: T; node: number; id: string; slot?: string;
}
export type InterfaceEvent = WireUiEvent;
export type SnapshotEvent =
  | { snapshot: 'containers'; of: ContainerSummary[] }
  | { snapshot: 'images'; of: ImageInventory }
  | { snapshot: 'volumes'; of: VolumeInventory }
  | { snapshot: 'networks'; of: NetworkInventory }
  | { snapshot: 'terminal'; of: TabSummary[] }
  | { snapshot: 'pane_changes'; of: PaneChange }
  | { snapshot: 'executions'; of: ExecutionList }
  | { snapshot: 'image_pulls'; of: ImagePullChange }
  | { snapshot: 'extensions'; of: ExtensionSummary[] }
  | { snapshot: 'extension_acquisitions'; of: ExtensionAcquisitionChange }
  | { snapshot: 'workspace_lifecycle'; of: WorkspaceLifecycleChange }
  | { snapshot: 'workspace_events'; of: WorkspaceEventBatch };
export type HostEvent = SnapshotEvent | PaneSelection | InterfaceEvent;
export function validateUiEvent(value: unknown): PaneSelection | InterfaceEvent;

export class ExtensionError extends Error {
  readonly kind: 'denied' | 'absent' | 'conflict' | 'failed' | 'unsupported';
  readonly capability?: string;
}

export class ExecutionOperationError extends Error {
  readonly executionId: string;
  readonly phase: 'wait' | 'logs';
  readonly cause: unknown;
  /** The authoritative completed summary when waiting succeeded and output retrieval failed. */
  readonly execution?: ExecutionSummary;
}

export class TerminalOperationError extends Error {
  readonly operation: 'open-tab';
  readonly result: Readonly<{ tab: string; title: string }>;
  readonly cause: unknown;
}

export interface ConnectOptions {
  path?: string;
  pendingLimit?: number;
  timeout?: number;
  connectTimeout?: number;
  onRows?: (request: unknown, channel: number) => void;
  onReply?: (reply: unknown) => void;
  onEvent?: (event: HostEvent, channel: number) => void;
  onEventError?: (error: unknown) => void;
  onClose?: (error: Error) => void;
}

export interface CallOptions {
  /**
   * Cancels without writing when already aborted. Aborting after the request
   * was written closes the ordered session and rejects every pending call.
   */
  signal?: AbortSignal;
}

export class Session {
  static connect(path?: string, handlers?: ConnectOptions): Promise<Session>;
  readonly ready: Promise<void>;
  readonly granted: readonly string[];
  readonly grantedCapabilities: readonly ExtensionCapability[];
  call<C extends WireCall>(method: C,
    ...args: WireRequestFor<C> extends { with: infer P }
      ? [params: P, options?: CallOptions]
      : [params?: undefined, options?: CallOptions]
  ): Promise<WireReplyFor<C>>;
  /** Round-trip a bounded opaque heartbeat without consuming ordered call replies. */
  ping(): Promise<void>;
  answer(channel: number, window: unknown): void;
  onEvent(listener: (event: HostEvent, channel: number) => void): () => boolean;
  close(): Promise<void>;
}

export function connect(options?: ConnectOptions): Promise<Session>;

/** A first frame rendered without loading a UI framework. */
export interface SurfaceBootstrap {
  readonly slot: string;
  readonly sequence: 1;
  readonly nextNode: 2;
  readonly bootstrapNode: 1;
}

export function bootstrapSurface(
  session: Session,
  options?: { title?: string; label?: string; primary?: boolean },
): Promise<SurfaceBootstrap>;

export interface WorkspaceApi {
  readonly granted: readonly string[];
  readonly grantedCapabilities: readonly ExtensionCapability[];
  /** Returns the complete typed facade with every call bound to this signal. */
  withSignal(signal: AbortSignal): WorkspaceApi;
  info(): Promise<WorkspaceInfo>;
  list(): Promise<WorkspaceState[]>;
  inspect(name: string): Promise<WorkspaceConfiguration>;
  create(configuration: WorkspaceConfiguration): Promise<WorkspaceConfiguration>;
  /** Assign identity to an imported generation-less workspace record. */
  adopt(configuration: WorkspaceConfiguration): Promise<WorkspaceConfiguration>;
  update(name: string, generation: string, configuration: WorkspaceConfiguration): Promise<WorkspaceConfiguration>;
  delete(name: string, generation: string): Promise<void>;
  start(name: string): Promise<void>;
  stop(name: string): Promise<void>;
  restart(name: string): Promise<void>;
  extensions: {
    list(): Promise<ExtensionSummary[]>;
    inspect(name: string): Promise<ExtensionSummary>;
    enable(name: string, imageDigest: string): Promise<void>;
    /** Arm inventory observation, enable this exact digest, then verify its durable enabled state. */
    enableAndWait(name: string, imageDigest: string, options?: { timeoutMs?: number }): Promise<
      | { changed: true; extension: ExtensionSummary }
      | { changed: false; name: string; image_digest: string }
    >;
    disable(name: string, imageDigest: string): Promise<void>;
    /** Arm inventory observation, disable this exact digest, then verify its durable standby state. */
    disableAndWait(name: string, imageDigest: string, options?: { timeoutMs?: number }): Promise<
      | { changed: true; extension: ExtensionSummary }
      | { changed: false; name: string; image_digest: string }
    >;
    retry(name: string, imageDigest: string): Promise<void>;
    /** Arm inventory, retry this exact faulted digest, then verify durable duty. */
    retryAndWait(name: string, imageDigest: string, options?: { timeoutMs?: number }): Promise<
      | { changed: true; extension: ExtensionSummary }
      | { changed: false; name: string; image_digest: string }
    >;
    remove(name: string, generation: string): Promise<void>;
    /** Arm inventory, remove this exact digest, then prove that digest is durably absent. */
    removeAndWait(name: string, imageDigest: string, options?: { timeoutMs?: number }): Promise<
      | { changed: true; removed: { name: string; image_digest: string }; replacement: ExtensionSummary | null }
      | { changed: false; name: string; image_digest: string }
    >;
    startAcquisition(reference: string): Promise<ExtensionAcquisitionJob>;
    acquisition(job: string): Promise<ExtensionAcquisitionStatus>;
    /** Wait for this exact acquisition job revision to advance, then return its authoritative status. */
    waitForAcquisition(job: string, afterRevision: number, options?: { timeoutMs?: number }): Promise<
      | { changed: true; status: ExtensionAcquisitionStatus }
      | { changed: false; job: string; revision: number }
    >;
    cancelAcquisition(job: string, revision: number): Promise<void>;
    install(job: string, revision: number, granted: ExtensionCapability[]): Promise<ExtensionSummary>;
    /** Inspect the exact ready revision, arm inventory, install it, then verify its published identity. */
    installAndWait(job: string, revision: number, granted: ExtensionCapability[], options?: { timeoutMs?: number }): Promise<
      | { changed: true; extension: ExtensionSummary }
      | { changed: false; name: string; image_digest: string; revision: number }
    >;
    update(job: string, revision: number, granted: ExtensionCapability[]): Promise<ExtensionSummary>;
    /** Inspect the exact ready revision, arm inventory, update it, then verify its published identity. */
    updateAndWait(job: string, revision: number, granted: ExtensionCapability[], options?: { timeoutMs?: number }): Promise<
      | { changed: true; extension: ExtensionSummary }
      | { changed: false; name: string; image_digest: string; revision: number }
    >;
    /** Enabled manifest declarations, independent of whether a provider currently occupies a pane. */
    providers(): Promise<ExtensionProviderCatalogue>;
    /** Wait for the extension lifecycle cursor to change, then return its enabled provider catalogue. */
    waitForProviders(after: Pick<ExtensionSummary, 'name' | 'image_digest' | 'status'>, options?: { timeoutMs?: number }): Promise<{ changed: boolean; extension?: Pick<ExtensionSummary, 'name' | 'image_digest' | 'status'> | null; catalogue?: ExtensionProviderCatalogue; after?: Pick<ExtensionSummary, 'name' | 'image_digest' | 'status'> }>;
    /** Wait for an actually mounted provider occupant, or its removal, using an exact prior pane cursor. */
    waitForProviderMount(extension: string, provider: string, options?: {
      state?: 'mounted' | 'unmounted';
      after?: Pick<InspectablePane, 'slot' | 'generation' | 'revision'> | null;
      timeoutMs?: number;
    }): Promise<{ changed: boolean; state: 'mounted' | 'unmounted'; pane?: InspectablePane | null; truncated?: false; after?: Pick<InspectablePane, 'slot' | 'generation' | 'revision'> | null }>;
  };
  containers: {
    list(): Promise<ContainerSummary[]>;
    inspect(id: string): Promise<ContainerSummary>;
    processes(id: string): Promise<ProcessList>;
    logs(id: string, streams?: { stdout?: boolean; stderr?: boolean }): Promise<ContainerOutput>;
    execution(id: string): Promise<ExecutionSummary>;
    executions(): Promise<ExecutionList>;
    executionLogs(id: string, streams?: { stdout?: boolean; stderr?: boolean }): Promise<ContainerOutput>;
    waitExecution(id: string, options?: { timeoutMs?: number }): Promise<ExecutionSummary>;
    /** Execute, wait for completion, then fetch bounded output without auto-removing the execution record. */
    execAndWait(id: string, options: {
      command: string[]; user?: string; workingDirectory?: string; timeoutMs?: number;
      stdout?: boolean; stderr?: boolean;
    }): Promise<{ execution: ExecutionSummary; output: ContainerOutput }>;
    signalExecution(id: string, signal: string): Promise<void>;
    /** Arm and verify the exact execution cursor before signaling, then await its requested transition. */
    signalExecutionAndWait(id: string, signal: string,
      after: Pick<ExecutionSummary, 'running' | 'exit_code' | 'pid'>,
      options?: { state?: 'changed' | 'exited'; timeoutMs?: number }): Promise<
        | { changed: true; execution: ExecutionSummary }
        | { changed: false; id: string; state: 'changed' | 'exited'; after: Pick<ExecutionSummary, 'running' | 'exit_code' | 'pid'> }
      >;
    removeExecution(id: string): Promise<void>;
    /** Inspect the exact finished cursor, remove it, then prove absence from a later complete execution catalogue. */
    removeExecutionAndWait(id: string, after: Pick<ExecutionSummary, 'running' | 'exit_code' | 'pid'>,
      options?: { timeoutMs?: number }): Promise<{ changed: boolean; id: string }>;
    create(configuration: ContainerCreateSpec): Promise<string>;
    /** Backwards-compatible shorthand for an image and optional container name. */
    create(image: string, name?: string): Promise<string>;
    start(id: string): Promise<void>;
    /** Arm bounded inventory, start an immutable ID, then accept only a later running snapshot. */
    startAndWait(id: string, options?: { timeoutMs?: number }): Promise<
      | { changed: true; container: ContainerSummary }
      | { changed: false; id: string; state: 'running' }
    >;
    stop(id: string): Promise<void>;
    /** Arm bounded inventory, stop an immutable ID, then accept only a later exited snapshot. */
    stopAndWait(id: string, options?: { timeoutMs?: number }): Promise<
      | { changed: true; container: ContainerSummary }
      | { changed: false; id: string; state: 'exited' }
    >;
    remove(id: string): Promise<void>;
    /** Remove an immutable ID and accept absence only from a later complete bounded inventory. */
    removeAndWait(id: string, options?: { timeoutMs?: number }): Promise<{ changed: boolean; id: string }>;
    pause(id: string): Promise<void>;
    unpause(id: string): Promise<void>;
    restart(id: string): Promise<void>;
    /** Restart only after observing a generation; resolves on the same ID running at a newer generation. */
    restartAndWait(id: string, generation: number, options?: { timeoutMs?: number }): Promise<
      | { changed: true; container: ContainerSummary }
      | { changed: false; id: string; generation: number }
    >;
    rename(id: string, name: string): Promise<void>;
    kill(id: string, signal: string): Promise<void>;
    exec(id: string, options: { command: string[]; user?: string; workingDirectory?: string }): Promise<string>;
    attachTerminal(id: string, command: string[]): Promise<string>;
  };
  images: { inventory(): Promise<ImageInventory>; list(): Promise<ImageSummary[]>; pull(reference: string): Promise<ImageSummary>; inspect(reference: string): Promise<ImageDetails>; startPull(reference: string): Promise<ImagePullJob>; pullStatus(job: string): Promise<ImagePullStatus>; cancelPull(job: string): Promise<void>; remove(reference: string): Promise<void>; removeAndWait(reference: string, options?: { timeoutMs?: number }): Promise<{ changed: boolean; id: string }>; prune(): Promise<ImagePruneResult> };
  volumes: {
    inventory(): Promise<VolumeInventory>;
    list(): Promise<VolumeSummary[]>;
    inspect(name: string): Promise<VolumeSummary>;
    create(name: string): Promise<VolumeSummary>;
    remove(name: string, imageDigest: string): Promise<void>;
    removeAndWait(name: string, generation: string, options?: { timeoutMs?: number }): Promise<{ changed: boolean; name: string; generation: string }>;
  };
  networks: {
    inventory(): Promise<NetworkInventory>;
    list(): Promise<NetworkSummary[]>;
    inspect(reference: string): Promise<NetworkSummary>;
    create(name: string): Promise<string>;
    remove(reference: string): Promise<void>;
    removeAndWait(reference: string, options?: { timeoutMs?: number }): Promise<{ changed: boolean; id: string }>;
    connect(reference: string, container: string, options?: { aliases?: readonly string[] }): Promise<void>;
    disconnect(reference: string, container: string): Promise<void>;
  };
  terminal: {
    panes(): Promise<PaneInventory>;
    tabs(): Promise<TabSummary[]>;
    topology(): Promise<TerminalTopology>;
    openTab(title: string): Promise<string>;
    pinTab(tab: string, pinned?: boolean): Promise<void>;
    /** Arm pane observation before opening the session-owned tab and verify its exact returned identity. Observation failures retain the created tab in TerminalOperationError. */
    openTabAndWait(title: string, options?: { timeoutMs?: number }): Promise<
      | { changed: true; tab: string; pane: InspectablePane }
      | { changed: false; tab: string; title: string }
    >;
    split(slot: string, division: Division): Promise<string>;
    splitObserved(slot: string, generation: number, revision: number, division: Division): Promise<string>;
    /** Arm pane changes before a CAS split, then verify the returned child slot in bounded inventory. */
    splitAndWait(slot: string, generation: number, revision: number, division: Division,
      options?: { timeoutMs?: number }): Promise<
        | { changed: true; pane: InspectablePane }
        | { changed: false; slot: string; after: { generation: number; revision: number } }
      >;
    spawn(slot: string, command: string[]): Promise<void>;
    spawnObserved(slot: string, generation: number, revision: number, command: string[]): Promise<void>;
    /** Arm and read before CAS spawn, then return a later bounded terminal screen revision. */
    spawnAndWait(slot: string, generation: number, revision: number, command: string[],
      options?: { lines?: number; timeoutMs?: number }): Promise<
        | { changed: true; command: string[]; before: PaneText; after: PaneText }
        | { changed: false; command: string[]; before: PaneText }
      >;
    read(slot: string, lines?: number): Promise<PaneText>;
    semantics(slot: string): Promise<PaneSemanticTree>;
    /** Discover the pane kind and return terminal screen text or bounded semantic XML. */
    toText(slot: string, options?: { lines?: number }): Promise<ReadablePane>;
    /** Wait for a pane cursor to change, then return a fresh bounded text projection. */
    waitForText(slot: string, after: Pick<PaneText | PaneSemanticTree, 'generation' | 'revision'>, options?: {
      lines?: number;
      timeoutMs?: number;
    }): Promise<{ changed: true; readable: ReadablePane } | { changed: false; after: Pick<PaneText | PaneSemanticTree, 'generation' | 'revision'> }>;
    act(slot: string, action: PaneSemanticAction): Promise<void>;
    /** Arm pane observation, perform one revision-bound semantic action, then read its changed projection. */
    actAndWait(slot: string, action: PaneSemanticAction, options?: { lines?: number; timeoutMs?: number }): Promise<
      | { changed: true; readable: ReadablePane }
      | { changed: false; after: { generation: number; revision: number } }
    >;
    /** Inspect and validate an enabled advertised action, then invoke it with that exact semantic cursor. */
    inspectAndAct(slot: string, proposal: { node: number; action: SemanticActionKind; value?: string | null },
      options?: { timeoutMs?: number }): Promise<
        | { changed: true; before: { snapshot: PaneSemanticTree; text: string }; after: { snapshot: PaneSemanticTree; text: string } }
        | { changed: false; before: { snapshot: PaneSemanticTree; text: string } }
      >;
    writeInput(slot: string, generation: number, revision: number, input: string | Iterable<number>): Promise<void>;
    /** Arm and read before CAS input, then return a later bounded terminal screen revision. */
    writeAndWait(slot: string, generation: number, revision: number, input: string | Iterable<number>,
      options?: { lines?: number; timeoutMs?: number }): Promise<
        | { changed: true; before: PaneText; after: PaneText }
        | { changed: false; before: PaneText }
      >;
    resizeGrid(slot: string, columns: number, rows: number): Promise<void>;
    resizeGridObserved(slot: string, generation: number, revision: number, columns: number, rows: number): Promise<void>;
    /** Arm and read before CAS resize, then verify the exact terminal grid on a later screen revision. */
    resizeGridAndWait(slot: string, generation: number, revision: number, columns: number, rows: number,
      options?: { lines?: number; timeoutMs?: number }): Promise<
        | { changed: true; columns: number; rows: number; before: PaneText; after: PaneText }
        | { changed: false; columns: number; rows: number; before: PaneText }
      >;
    close(slot: string): Promise<void>;
    closeObserved(slot: string, generation: number, revision: number): Promise<void>;
    /** Arm pane changes before CAS close and prove absence only from a complete inventory. */
    closeAndWait(slot: string, generation: number, revision: number,
      options?: { timeoutMs?: number }): Promise<
        | { changed: true; slot: string }
        | { changed: false; slot: string; after: { generation: number; revision: number } }
      >;
    focus(slot: string): Promise<void>;
    focusObserved(slot: string, generation: number, revision: number): Promise<void>;
    /** Arm pane changes before CAS focus and verify the same pane is focused at an advanced revision. */
    focusAndWait(slot: string, generation: number, revision: number,
      options?: { timeoutMs?: number }): Promise<
        | { changed: true; pane: InspectablePane }
        | { changed: false; slot: string; after: { generation: number; revision: number } }
      >;
    retitle(slot: string, title: string): Promise<void>;
    retitleObserved(slot: string, generation: number, revision: number, title: string): Promise<void>;
    /** Arm pane changes before CAS retitle and verify the exact title and advanced revision. */
    retitleAndWait(slot: string, generation: number, revision: number, title: string,
      options?: { timeoutMs?: number }): Promise<
        | { changed: true; pane: InspectablePane }
        | { changed: false; title: string; after: { generation: number; revision: number } }
      >;
    ratio(slot: string, ratio: number): Promise<void>;
    ratioObserved(slot: string, generation: number, revision: number, ratio: number): Promise<void>;
    /** Arm pane observation before a CAS ratio change, then verify its advanced pane and resulting topology. */
    ratioAndWait(slot: string, generation: number, revision: number, ratio: number,
      options?: { timeoutMs?: number }): Promise<
        | { changed: true; ratio: number; actual: number; pane: InspectablePane }
        | { changed: false; ratio: number; after: { generation: number; revision: number } }
      >;
    switchOccupant(slot: string, generation: number, target: { kind: 'terminal' } | { kind: 'surface'; extension: string; provider: string }): Promise<void>;
    switchOccupantObserved(slot: string, generation: number, revision: number, target: { kind: 'terminal' } | { kind: 'surface'; extension: string; provider: string }): Promise<void>;
    /** Arm observation, perform an observed switch, and verify the exact resulting occupant. */
    switchOccupantAndWait(slot: string, generation: number, revision: number,
      target: { kind: 'terminal' } | { kind: 'surface'; extension: string; provider: string },
      options?: { timeoutMs?: number }): Promise<
        | { changed: true; pane: InspectablePane }
        | { changed: false; target: { kind: 'terminal' } | { kind: 'surface'; extension: string; provider: string }; after: { generation: number; revision: number } }
      >;
  };
  files: {
    list(path: string): Promise<FileEntry[]>;
    stat(path: string): Promise<FileEntry>;
    read(path: string): Promise<number[]>;
    readRange(path: string, offset?: number, limit?: number, observed?: string | null): Promise<FileRange>;
    write(path: string, contents: Iterable<number>): Promise<void>;
    createObserved(path: string, contents: Iterable<number>): Promise<string>;
    mkdir(path: string): Promise<void>;
    rename(from: string, to: string): Promise<void>;
    renameObserved(from: string, to: string, observed: string): Promise<string>;
    remove(path: string): Promise<void>;
    removeObserved(path: string, observed: string): Promise<void>;
  };
  subscribe(topic: Topic): Promise<void>;
  unsubscribe(topic: Topic): Promise<void>;
  watchPaneChanges(listener: (change: PaneChange) => void): Promise<() => Promise<void>>;
  watchContainers(listener: (containers: ContainerSummary[]) => void): Promise<() => Promise<void>>;
  watchContainerInventory(listener: (inventory: ContainerInventory) => void): Promise<() => Promise<void>>;
  watchImages(listener: (images: ImageSummary[]) => void): Promise<() => Promise<void>>;
  watchImageInventory(listener: (inventory: ImageInventory) => void): Promise<() => Promise<void>>;
  watchVolumes(listener: (volumes: VolumeSummary[]) => void): Promise<() => Promise<void>>;
  watchVolumeInventory(listener: (inventory: VolumeInventory) => void): Promise<() => Promise<void>>;
  watchNetworks(listener: (networks: NetworkSummary[]) => void): Promise<() => Promise<void>>;
  watchNetworkInventory(listener: (inventory: NetworkInventory) => void): Promise<() => Promise<void>>;
  watchTerminal(listener: (tabs: TabSummary[]) => void): Promise<() => Promise<void>>;
  watchExecutions(listener: (executions: ExecutionList) => void): Promise<() => Promise<void>>;
  watchImagePulls(listener: (change: ImagePullChange) => void): Promise<() => Promise<void>>;
  watchExtensions(listener: (extensions: ExtensionSummary[]) => void): Promise<() => Promise<void>>;
  watchExtensionAcquisitions(listener: (change: ExtensionAcquisitionChange) => void): Promise<() => Promise<void>>;
  watchWorkspaceLifecycle(listener: (change: WorkspaceLifecycleChange) => void): Promise<() => Promise<void>>;
  watchWorkspaceEvents(listener: (batch: WorkspaceEventBatch) => void): Promise<() => Promise<void>>;
}

export function requestCapability(call: string): string;

export function workspace(session: Session, options?: CallOptions): WorkspaceApi;
export const protocolSurface: Readonly<{
  requests: Readonly<Record<string,
    | Readonly<{ kind: 'facade' | 'subscription'; api: string }>
    | Readonly<{ kind: 'internal'; rationale: string }>>>;
  topics: Readonly<Record<Topic, Readonly<{ subscribe: 'subscribe'; unsubscribe: 'unsubscribe' }>>>;
}>;
export const protocolCoverage: Readonly<{
  available: Readonly<Record<string, readonly string[]>>;
  unavailable: Readonly<Record<string, readonly string[]>>;
}>;
export function semanticXml(tree: PaneSemanticTree): string;
export * from './generated-protocol.js';

import type { WireUiEvent } from './generated-protocol.js';

export type Topic = 'containers' | 'images' | 'volumes' | 'networks' | 'terminal' | 'pane-changes' | 'executions' | 'image-pulls' | 'extensions' | 'extension-acquisitions' | 'workspace-lifecycle' | 'workspace-events';
export type Division = 'beside' | 'below';
export interface WorkspaceInfo { name: string; architecture: string; image: string }
export interface ExtensionPaneProvider { id: string; title: string; icon: string | null }
export interface ExtensionSummary { name: string; image_digest: string; status: string; version?: string; enabled?: boolean; pane_providers?: ExtensionPaneProvider[] }
export interface ExtensionProviderDeclaration { extension: string; image_digest: string; version: string; status: string; id: string; title: string; icon: string | null }
export interface ExtensionProviderCatalogue { providers: ExtensionProviderDeclaration[]; truncated: boolean }
export type ExtensionCapability =
  | 'workspace-read' | 'workspace-control' | 'workspace-events'
  | 'container-read' | 'container-control' | 'container-attach' | 'image-read' | 'image-write'
  | 'volume-read' | 'volume-write' | 'network-read' | 'network-write'
  | 'terminal-read' | 'terminal-control' | 'terminal-output' | 'pane-observe'
  | 'pane-semantic-read' | 'pane-semantic-control' | 'extension-read'
  | 'extension-control' | 'extension-install' | 'filesystem-read'
  | 'filesystem-write' | 'interface';
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
export interface ContainerSummary { id: string; name: string; image: string; state: string; created: number }
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
export interface ImageDetails { id: string; references: string[]; created: string; size: number; os: string; architecture: string; entrypoint: string[]; command: string[]; working_directory: string; user: string }
export interface ImagePruneResult { deleted: number; space_reclaimed: number }
export interface ImagePullJob { job: string }
export interface ImagePullStatus { job: string; reference: string; revision: number; state: string; status: string | null; layer: string | null; current: number | null; total: number | null; image: ImageSummary | null; error: string | null }
export interface ImagePullChange { job: string; revision: number; state: string; coalesced: number }
export interface VolumeSummary { name: string; driver: string; generation: string }
export interface NetworkSummary { id: string; name: string; driver: string; scope: string }
export interface PaneSummary {
  slot: string;
  working_directory: string | null;
  command: string | null;
  occupant: 'terminal' | 'surface';
  provider: { extension: string; provider: string } | null;
}
export interface TabSummary { id: string; title: string; panes: PaneSummary[] }
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
export interface TabTopology { id: string; title: string; root: LayoutNode }
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
/** Protocol-1 interface spellings accepted from older hosts by the event router. */
export type LegacyInterfaceEvent =
  | { slot?: string; event: string; node: number; id: string; value?: unknown }
  | { slot?: string; event: Record<string, { node: number; id: string; value?: unknown }> };
export type SnapshotEvent =
  | { snapshot: 'containers'; of: ContainerSummary[] }
  | { snapshot: 'images'; of: ImageSummary[] }
  | { snapshot: 'volumes'; of: VolumeSummary[] }
  | { snapshot: 'networks'; of: NetworkSummary[] }
  | { snapshot: 'terminal'; of: TabSummary[] }
  | { snapshot: 'pane_changes'; of: PaneChange }
  | { snapshot: 'executions'; of: ExecutionList }
  | { snapshot: 'image_pulls'; of: ImagePullChange }
  | { snapshot: 'extensions'; of: ExtensionSummary[] }
  | { snapshot: 'extension_acquisitions'; of: ExtensionAcquisitionChange }
  | { snapshot: 'workspace_lifecycle'; of: WorkspaceLifecycleChange }
  | { snapshot: 'workspace_events'; of: WorkspaceEventBatch };
export type HostEvent = SnapshotEvent | PaneSelection | InterfaceEvent | LegacyInterfaceEvent;
export function validateUiEvent(value: unknown): PaneSelection | InterfaceEvent | LegacyInterfaceEvent;

export class ExtensionError extends Error {
  readonly kind: 'denied' | 'absent' | 'conflict' | 'failed' | 'unsupported';
  readonly capability?: string;
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
  call(method: string, params?: unknown, options?: CallOptions): Promise<unknown>;
  /** Round-trip a bounded opaque heartbeat without consuming ordered call replies. */
  ping(): Promise<void>;
  answer(channel: number, window: unknown): void;
  onEvent(listener: (event: HostEvent, channel: number) => void): () => boolean;
  close(): Promise<void>;
}

export function connect(options?: ConnectOptions): Promise<Session>;

export interface WorkspaceApi {
  readonly granted: readonly string[];
  readonly grantedCapabilities: readonly ExtensionCapability[];
  /** Returns the complete typed facade with every call bound to this signal. */
  withSignal(signal: AbortSignal): WorkspaceApi;
  info(): Promise<WorkspaceInfo>;
  list(): Promise<WorkspaceState[]>;
  inspect(name: string): Promise<WorkspaceConfiguration>;
  create(configuration: WorkspaceConfiguration): Promise<WorkspaceConfiguration>;
  /** Assign identity to the exact still-unchanged generation-less legacy record. */
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
    update(job: string, revision: number, granted: ExtensionCapability[]): Promise<ExtensionSummary>;
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
    signalExecution(id: string, signal: string): Promise<void>;
    removeExecution(id: string): Promise<void>;
    create(configuration: ContainerCreateSpec): Promise<string>;
    /** Backwards-compatible shorthand for an image and optional container name. */
    create(image: string, name?: string): Promise<string>;
    start(id: string): Promise<void>;
    stop(id: string): Promise<void>;
    remove(id: string): Promise<void>;
    pause(id: string): Promise<void>;
    unpause(id: string): Promise<void>;
    restart(id: string): Promise<void>;
    rename(id: string, name: string): Promise<void>;
    kill(id: string, signal: string): Promise<void>;
    exec(id: string, options: { command: string[]; user?: string; workingDirectory?: string }): Promise<string>;
    attachTerminal(id: string, command: string[]): Promise<string>;
  };
  images: { list(): Promise<ImageSummary[]>; pull(reference: string): Promise<ImageSummary>; inspect(reference: string): Promise<ImageDetails>; startPull(reference: string): Promise<ImagePullJob>; pullStatus(job: string): Promise<ImagePullStatus>; cancelPull(job: string): Promise<void>; remove(reference: string): Promise<void>; prune(): Promise<ImagePruneResult> };
  volumes: {
    list(): Promise<VolumeSummary[]>;
    inspect(name: string): Promise<VolumeSummary>;
    create(name: string): Promise<VolumeSummary>;
    remove(name: string, imageDigest: string): Promise<void>;
  };
  networks: {
    list(): Promise<NetworkSummary[]>;
    inspect(reference: string): Promise<NetworkSummary>;
    create(name: string): Promise<string>;
    remove(reference: string): Promise<void>;
    connect(reference: string, container: string, options?: { aliases?: readonly string[] }): Promise<void>;
    disconnect(reference: string, container: string): Promise<void>;
  };
  terminal: {
    panes(): Promise<PaneInventory>;
    tabs(): Promise<TabSummary[]>;
    topology(): Promise<TerminalTopology>;
    openTab(title: string): Promise<string>;
    split(slot: string, division: Division): Promise<string>;
    splitObserved(slot: string, generation: number, revision: number, division: Division): Promise<string>;
    spawn(slot: string, command: string[]): Promise<void>;
    spawnObserved(slot: string, generation: number, revision: number, command: string[]): Promise<void>;
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
    writeInput(slot: string, generation: number, revision: number, input: string | Iterable<number>): Promise<void>;
    resizeGrid(slot: string, columns: number, rows: number): Promise<void>;
    resizeGridObserved(slot: string, generation: number, revision: number, columns: number, rows: number): Promise<void>;
    close(slot: string): Promise<void>;
    closeObserved(slot: string, generation: number, revision: number): Promise<void>;
    focus(slot: string): Promise<void>;
    focusObserved(slot: string, generation: number, revision: number): Promise<void>;
    retitle(slot: string, title: string): Promise<void>;
    retitleObserved(slot: string, generation: number, revision: number, title: string): Promise<void>;
    ratio(slot: string, ratio: number): Promise<void>;
    ratioObserved(slot: string, generation: number, revision: number, ratio: number): Promise<void>;
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
  watchImages(listener: (images: ImageSummary[]) => void): Promise<() => Promise<void>>;
  watchVolumes(listener: (volumes: VolumeSummary[]) => void): Promise<() => Promise<void>>;
  watchNetworks(listener: (networks: NetworkSummary[]) => void): Promise<() => Promise<void>>;
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

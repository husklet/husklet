import type {
  ContainerOutput, ContainerSummary, ExecutionSummary, ImageDetails, InterfaceSourceMutation,
  NetworkSummary, ProcessList, VolumeSummary,
} from '@husklet/react';

type DetailCell = { Text: string } | { Code: string };
type DetailRow = { id: number; cells: DetailCell[] };
type RowRequest = { source: number; version: number; id: number; range: { start: number; count: number } };
type RowWindow = { source: number; version: number; request: number; range: RowRequest['range']; rows: DetailRow[] };
type DetailSender = (mutation: InterfaceSourceMutation) => Promise<void>;
type ResourceReference = { id?: unknown; name?: unknown } | null | undefined;

function detailRows(values: ReadonlyArray<readonly [string, unknown]>): DetailRow[] {
  return values
    .filter(([, value]) => value !== null && value !== undefined && String(value).length > 0)
    .map(([key, value], index) => ({ id: index + 1, cells: [{ Text: key }, { Code: String(value) }] }));
}

function rowRequest(value: unknown): RowRequest | null {
  if (!value || typeof value !== 'object') return null;
  const candidate = value as Partial<RowRequest>;
  if (!Number.isSafeInteger(candidate.source) || !Number.isSafeInteger(candidate.version)
    || !Number.isSafeInteger(candidate.id) || !candidate.range
    || !Number.isSafeInteger(candidate.range.start) || candidate.range.start < 0
    || !Number.isSafeInteger(candidate.range.count) || candidate.range.count < 0) return null;
  return candidate as RowRequest;
}
/** Maximum records mounted into a native tree at once. */
export const RECORD_LIMIT = 200;
export const LOG_LIMIT = 400;

const CONTAINER_NAME = /^[A-Za-z0-9][A-Za-z0-9_.-]{0,127}$/;

/** The native container-name grammar, expressed as a user-facing validation result. */
export function containerNameError(name: unknown): string {
  return typeof name === 'string' && CONTAINER_NAME.test(name)
    ? ''
    : 'Container name must contain 1–128 ASCII letters, digits, underscores, periods, or hyphens and start with a letter or digit.';
}

/** A bounded view plus the number honestly omitted. */
export function bounded<T>(records: readonly T[] | null | undefined, limit = RECORD_LIMIT): { records: T[]; omitted: number } {
  const all = Array.isArray(records) ? records : [];
  return { records: all.slice(0, limit), omitted: Math.max(0, all.length - limit) };
}

export function endpointAliases(value: unknown): string[] {
  if (typeof value !== 'string' || value.trim().length === 0) return [];
  const aliases = value.split(',').map((alias) => alias.trim());
  const valid = aliases.length <= 64
    && new Set(aliases).size === aliases.length
    && aliases.every((alias) => alias.length >= 1 && alias.length <= 253
      && [...alias].every((character, index) => /[A-Za-z0-9]/.test(character)
        || (index > 0 && '_.-'.includes(character))));
  if (!valid) throw new TypeError('Network endpoint aliases must be at most 64 unique, 1..=253-byte ASCII endpoint names.');
  return aliases;
}

export function immutableContainerId(value: string): boolean {
  return /^(?:[0-9a-f]{32}|[0-9a-f]{64})$/.test(value);
}

export function boundedMessage(value: unknown, limit = 512): string {
  const message = value instanceof Error ? value.message : String(value ?? '');
  return message.length <= limit ? message : `${message.slice(0, limit)}…`;
}

export function shortId(value: unknown): string {
  const id = String(value ?? '—');
  return id.length > 12 ? id.slice(0, 12) : id;
}

/** Stable daemon reference: prefer an opaque ID, then the human name. */
export function resourceReference(resource: ResourceReference): string {
  return String(resource?.id || resource?.name || '');
}

export const IMAGE_DETAIL_SOURCE = 201;
export const IMAGE_DETAIL_LIMIT = 64;
export const IMAGE_DETAIL_WINDOW_LIMIT = 4;
export const CONTAINER_DETAIL_SOURCE = 202;
export const CONTAINER_DETAIL_WINDOW_LIMIT = 4;
export const EXECUTION_DETAIL_SOURCE = 203;
export const EXECUTION_DETAIL_WINDOW_LIMIT = 4;
export const NETWORK_DETAIL_SOURCE = 204;
export const NETWORK_DETAIL_WINDOW_LIMIT = 4;
export const VOLUME_DETAIL_SOURCE = 205;
export const VOLUME_DETAIL_WINDOW_LIMIT = 2;

/** Windowed rows derived only from the public typed ImageDetails contract. */
export class ImageDetailsSource {
  private readonly send: DetailSender;
  version: number;
  private rows: DetailRow[];
  generated: number;

  constructor(send: DetailSender = async () => {}) {
    this.send = send;
    this.version = 0;
    this.rows = [];
    this.generated = 0;
  }

  async replace(details: ImageDetails | null): Promise<number> {
    const values: Array<[string, unknown]> = [
      ['ID', details?.id],
      ['References', details?.references?.join(', ')],
      ['Created', details?.created],
      ['Size', details && Number.isFinite(details.size) ? bytes(details.size) : null],
      ['Operating system', details?.os],
      ['Architecture', details?.architecture],
      ['Entrypoint', details?.entrypoint?.join(' ')],
      ['Command', details?.command?.join(' ')],
      ['Working directory', details?.working_directory],
      ['User', details && 'user' in details ? details.user || 'default user' : null],
    ];
    this.rows = detailRows(values).slice(0, IMAGE_DETAIL_LIMIT);
    this.version += 1;
    await this.send({ Length: { source: IMAGE_DETAIL_SOURCE, version: this.version, rows: this.rows.length } });
    return this.rows.length;
  }

  answer(value: unknown): RowWindow | null {
    const request = rowRequest(value);
    if (!request) return null;
    if (request.source !== IMAGE_DETAIL_SOURCE || request.version !== this.version) return null;
    const count = Math.min(request.range.count, IMAGE_DETAIL_WINDOW_LIMIT,
      Math.max(0, this.rows.length - request.range.start));
    const rows = this.rows.slice(request.range.start, request.range.start + count);
    this.generated += rows.length;
    return { source: IMAGE_DETAIL_SOURCE, version: this.version, request: request.id, range: request.range, rows };
  }
}

/** Windowed rows derived only from the public typed ContainerSummary contract. */
export class ContainerDetailsSource {
  private readonly send: DetailSender;
  version: number;
  private rows: DetailRow[];

  constructor(send: DetailSender = async () => {}) {
    this.send = send;
    this.version = 0;
    this.rows = [];
  }

  async replace(details: ContainerSummary | null): Promise<number> {
    const values: Array<[string, unknown]> = [
      ['Immutable ID', details?.id],
      ['Name', details?.name],
      ['State', details?.state],
      ['Image', details?.image],
      ['Created', details && Number.isFinite(details.created) ? String(details.created) : null],
    ];
    this.rows = detailRows(values);
    this.version += 1;
    await this.send({ Length: { source: CONTAINER_DETAIL_SOURCE, version: this.version, rows: this.rows.length } });
    return this.rows.length;
  }

  answer(value: unknown): RowWindow | null {
    const request = rowRequest(value);
    if (!request) return null;
    if (request.source !== CONTAINER_DETAIL_SOURCE || request.version !== this.version) return null;
    const count = Math.min(request.range.count, CONTAINER_DETAIL_WINDOW_LIMIT,
      Math.max(0, this.rows.length - request.range.start));
    return { source: CONTAINER_DETAIL_SOURCE, version: this.version, request: request.id,
      range: request.range, rows: this.rows.slice(request.range.start, request.range.start + count) };
  }
}

/** Windowed rows from the typed ExecutionSummary contract. */
export class ExecutionDetailsSource {
  private readonly send: DetailSender;
  version: number;
  private rows: DetailRow[];

  constructor(send: DetailSender = async () => {}) {
    this.send = send;
    this.version = 0;
    this.rows = [];
  }

  async replace(details: ExecutionSummary | null): Promise<number> {
    const values: Array<[string, unknown]> = [
      ['Execution ID', details?.id],
      ['Container ID', details?.container_id],
      ['State', details && 'running' in details ? details.running ? 'running' : 'exited' : null],
      ['Exit code', details && 'exit_code' in details && !details.running ? String(details.exit_code) : null],
      ['Process ID', details && details.pid > 0 ? String(details.pid) : null],
      ['Command', details?.command?.join(' ')],
      ['User', details && 'user' in details ? details.user || 'default user' : null],
    ];
    this.rows = detailRows(values);
    this.version += 1;
    await this.send({ Length: { source: EXECUTION_DETAIL_SOURCE, version: this.version, rows: this.rows.length } });
    return this.rows.length;
  }

  answer(value: unknown): RowWindow | null {
    const request = rowRequest(value);
    if (!request) return null;
    if (request.source !== EXECUTION_DETAIL_SOURCE || request.version !== this.version) return null;
    const count = Math.min(request.range.count, EXECUTION_DETAIL_WINDOW_LIMIT,
      Math.max(0, this.rows.length - request.range.start));
    return { source: EXECUTION_DETAIL_SOURCE, version: this.version, request: request.id,
      range: request.range, rows: this.rows.slice(request.range.start, request.range.start + count) };
  }
}

/** Windowed rows from the typed NetworkSummary contract. */
export class NetworkDetailsSource {
  private readonly send: DetailSender;
  version: number;
  private rows: DetailRow[];

  constructor(send: DetailSender = async () => {}) {
    this.send = send;
    this.version = 0;
    this.rows = [];
  }

  async replace(details: NetworkSummary | null): Promise<number> {
    this.rows = detailRows([
      ['Network ID', details?.id],
      ['Name', details?.name],
      ['Driver', details?.driver],
      ['Scope', details?.scope],
    ]);
    this.version += 1;
    await this.send({ Length: { source: NETWORK_DETAIL_SOURCE, version: this.version, rows: this.rows.length } });
    return this.rows.length;
  }

  answer(value: unknown): RowWindow | null {
    const request = rowRequest(value);
    if (!request) return null;
    if (request.source !== NETWORK_DETAIL_SOURCE || request.version !== this.version) return null;
    const count = Math.min(request.range.count, NETWORK_DETAIL_WINDOW_LIMIT,
      Math.max(0, this.rows.length - request.range.start));
    return { source: NETWORK_DETAIL_SOURCE, version: this.version, request: request.id,
      range: request.range, rows: this.rows.slice(request.range.start, request.range.start + count) };
  }
}

/** Windowed rows from the deliberately small typed VolumeSummary contract. */
export class VolumeDetailsSource {
  private readonly send: DetailSender;
  version: number;
  private rows: DetailRow[];

  constructor(send: DetailSender = async () => {}) {
    this.send = send;
    this.version = 0;
    this.rows = [];
  }

  async replace(details: VolumeSummary | null): Promise<number> {
    this.rows = detailRows([
      ['Name', details?.name],
      ['Driver', details?.driver],
    ]);
    this.version += 1;
    await this.send({ Length: { source: VOLUME_DETAIL_SOURCE, version: this.version, rows: this.rows.length } });
    return this.rows.length;
  }

  answer(value: unknown): RowWindow | null {
    const request = rowRequest(value);
    if (!request) return null;
    if (request.source !== VOLUME_DETAIL_SOURCE || request.version !== this.version) return null;
    const count = Math.min(request.range.count, VOLUME_DETAIL_WINDOW_LIMIT,
      Math.max(0, this.rows.length - request.range.start));
    return { source: VOLUME_DETAIL_SOURCE, version: this.version, request: request.id,
      range: request.range, rows: this.rows.slice(request.range.start, request.range.start + count) };
  }
}

export function bytes(value: unknown): string {
  const amount = Number(value ?? 0);
  if (!Number.isFinite(amount) || amount < 1) return '0 B';
  const units = ['B', 'KiB', 'MiB', 'GiB', 'TiB'];
  const rank = Math.min(Math.floor(Math.log(amount) / Math.log(1024)), units.length - 1);
  return `${(amount / 1024 ** rank).toFixed(rank === 0 ? 0 : 1)} ${units[rank]}`;
}

export function logText(log: ContainerOutput | Uint8Array | readonly number[] | string | null | undefined): string {
  if (typeof log === 'string') return log;
  if (log instanceof Uint8Array || Array.isArray(log)) return new TextDecoder().decode(Uint8Array.from(log));
  if (log && typeof log === 'object' && 'stdout' in log) {
    return [log.stdout, log.stderr].filter(Boolean).map((stream) => logText(stream)).join('\n');
  }
  return '';
}

/** Turns the protocol's title + matrix process list into labelled cells. */
export function processRows(list: ProcessList, container: string): Array<{ container: string; cells: Record<string, string>; values: string[] }> {
  const titles = Array.isArray(list?.titles) ? list.titles.map(String) : [];
  const rows = Array.isArray(list?.processes) ? list.processes : [];
  return rows.map((cells) => ({
    container,
    cells: Object.fromEntries(titles.map((title, index) => [title, String(cells?.[index] ?? '')])),
    values: Array.isArray(cells) ? cells.map(String) : [],
  }));
}

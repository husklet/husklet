import type {
  ContainerOutput,
  ContainerSummary,
  ExecutionSummary,
  ImageDetails,
  InterfaceSourceMutation,
  NetworkSummary,
  ProcessList,
  ColumnSpec,
  SortReport,
  VolumeSummary,
} from '@husklet/react';

type DetailCell = { Text: string } | { Code: string };
type DetailRow = { key: number; cells: DetailCell[] };
type RowRequest = {
  source: number;
  version: number;
  id: number;
  range: { start: number; count: number };
};
type RowWindow = {
  source: number;
  version: number;
  request: number;
  range: RowRequest['range'];
  rows: DetailRow[];
};
type DetailSender = (mutation: InterfaceSourceMutation) => Promise<void>;
type ResourceReference = { id?: unknown; name?: unknown } | null | undefined;

function detailRows(values: ReadonlyArray<readonly [string, unknown]>): DetailRow[] {
  return values
    .filter(([, value]) => value !== null && value !== undefined && String(value).length > 0)
    .map(([key, value], index) => ({
      key: index + 1,
      cells: [{ Text: key }, { Code: String(value) }],
    }));
}

function rowRequest(value: unknown): RowRequest | null {
  if (!value || typeof value !== 'object') return null;
  const candidate = value as Partial<RowRequest>;
  if (
    !Number.isSafeInteger(candidate.source) ||
    !Number.isSafeInteger(candidate.version) ||
    !Number.isSafeInteger(candidate.id) ||
    !candidate.range ||
    !Number.isSafeInteger(candidate.range.start) ||
    candidate.range.start < 0 ||
    !Number.isSafeInteger(candidate.range.count) ||
    candidate.range.count < 0
  )
    return null;
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
export function bounded<T>(
  records: readonly T[] | null | undefined,
  limit = RECORD_LIMIT,
): { records: T[]; omitted: number } {
  const all = Array.isArray(records) ? records : [];
  return { records: all.slice(0, limit), omitted: Math.max(0, all.length - limit) };
}

export function endpointAliases(value: unknown): string[] {
  if (typeof value !== 'string' || value.trim().length === 0) return [];
  const aliases = value.split(',').map((alias) => alias.trim());
  const valid =
    aliases.length <= 64 &&
    new Set(aliases).size === aliases.length &&
    aliases.every(
      (alias) =>
        alias.length >= 1 &&
        alias.length <= 253 &&
        [...alias].every(
          (character, index) =>
            /[A-Za-z0-9]/.test(character) || (index > 0 && '_.-'.includes(character)),
        ),
    );
  if (!valid)
    throw new TypeError(
      'Network endpoint aliases must be at most 64 unique, 1..=253-byte ASCII endpoint names.',
    );
  return aliases;
}

export function immutableContainerId(value: string): boolean {
  return /^(?:[0-9a-f]{32}|[0-9a-f]{64})$/.test(value);
}

export function boundedMessage(value: unknown, limit = 512): string {
  const raw = value instanceof Error ? value.message : String(value ?? '');
  const message = friendlyTransportMessage(raw);
  return message.length <= limit ? message : `${message.slice(0, limit)}…`;
}

function friendlyTransportMessage(message: string): string {
  if (/expected frame \d+, received \d+/i.test(message)) {
    return 'Connection to Husklet was interrupted. Retry the operation.';
  }
  if (/extension catalogue transport closed/i.test(message)) {
    return 'The extension catalogue connection closed before it completed. Retry the catalogue.';
  }
  return message;
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
export const PROCESS_TABLE_SOURCE = 206;
export const PROCESS_TABLE_WINDOW_LIMIT = 128;

export type ProcessRecord = {
  container: string;
  cells: Record<string, string>;
  values: string[];
};

type ProcessColumn = ColumnSpec & { key: string };
type ProcessTableRow = { key: number; cells: Array<{ Text: string }> };

const PROCESS_COLUMNS = {
  container: {
    key: 'container',
    title: 'Container',
    width: { chars: 18 },
    sortable: true,
    identity: true,
  },
  pid: { key: 'pid', title: 'PID', width: { chars: 8 }, align: 'end', sortable: true },
  user: {
    key: 'user',
    title: 'User',
    width: { chars: 14 },
    sortable: true,
    importance: 'optional',
  },
  cpu: {
    key: 'cpu',
    title: 'CPU',
    width: { chars: 9 },
    align: 'end',
    sortable: true,
    importance: 'optional',
  },
  memory: {
    key: 'memory',
    title: 'Memory',
    width: { chars: 12 },
    align: 'end',
    sortable: true,
    importance: 'optional',
  },
  command: { key: 'command', title: 'Command', width: 'fill', sortable: true },
} as const satisfies Record<string, ProcessColumn>;

const PROCESS_ALIASES = {
  pid: ['PID', 'Pid', 'pid'],
  user: ['USER', 'User', 'user', 'UID'],
  cpu: ['CPU', '%CPU', 'CPU%', 'cpu'],
  memory: ['MEMORY', 'Memory', 'memory', 'MEM', '%MEM', 'MEM%', 'RSS', 'VSZ'],
  command: ['COMMAND', 'Command', 'command', 'CMD', 'Cmd', 'cmd'],
} as const;

function processValue(record: ProcessRecord, key: keyof typeof PROCESS_ALIASES): string {
  for (const alias of PROCESS_ALIASES[key]) {
    const value = record.cells[alias];
    if (value !== undefined) return value;
  }
  return key === 'command' ? (record.values.at(-1) ?? '') : '';
}

/** Stable, compact columns; optional metrics appear only when the daemon supplied them. */
export function processTableSchema(records: readonly ProcessRecord[]): readonly ProcessColumn[] {
  return [
    PROCESS_COLUMNS.container,
    PROCESS_COLUMNS.pid,
    ...(records.some((record) => processValue(record, 'user')) ? [PROCESS_COLUMNS.user] : []),
    ...(records.some((record) => processValue(record, 'cpu')) ? [PROCESS_COLUMNS.cpu] : []),
    ...(records.some((record) => processValue(record, 'memory')) ? [PROCESS_COLUMNS.memory] : []),
    PROCESS_COLUMNS.command,
  ];
}

/** A revisioned, filtered and sorted process snapshot served only in requested windows. */
export class ProcessTableSource {
  private readonly send: DetailSender;
  version: number;
  private rows: ProcessTableRow[];
  generated: number;

  constructor(send: DetailSender = async () => {}) {
    this.send = send;
    this.version = 0;
    this.rows = [];
    this.generated = 0;
  }

  async replace(
    records: readonly ProcessRecord[],
    schema: readonly ProcessColumn[],
    filter = '',
    sort = 'container',
    descending = false,
  ): Promise<number> {
    const query = filter.trim().toLocaleLowerCase();
    const selected = records
      .map((record, index) => ({ record, key: index + 1 }))
      .filter(({ record }) =>
        query
          ? [record.container, ...record.values].some((value) =>
              value.toLocaleLowerCase().includes(query),
            )
          : true,
      );
    const value = (record: ProcessRecord, key: string) =>
      key === 'container'
        ? record.container
        : processValue(record, key as keyof typeof PROCESS_ALIASES);
    const numeric = new Set(['pid', 'cpu', 'memory']);
    selected.sort((left, right) => {
      const a = value(left.record, sort);
      const b = value(right.record, sort);
      const order = numeric.has(sort)
        ? (Number.parseFloat(a) || 0) - (Number.parseFloat(b) || 0)
        : a.localeCompare(b, undefined, { numeric: true, sensitivity: 'base' });
      return descending ? -order : order;
    });
    this.rows = selected.map(({ record, key }) => ({
      key,
      cells: schema.map(({ key }) => ({ Text: value(record, key) || '—' })),
    }));
    this.version += 1;
    await this.send({
      Length: { source: PROCESS_TABLE_SOURCE, version: this.version, rows: this.rows.length },
    });
    return this.rows.length;
  }

  accepts(event: SortReport): boolean {
    return (
      event.source === PROCESS_TABLE_SOURCE &&
      event.version === this.version &&
      Object.hasOwn(PROCESS_COLUMNS, event.column)
    );
  }

  answer(value: unknown): RowWindow | null {
    const request = rowRequest(value);
    if (!request) return null;
    if (request.source !== PROCESS_TABLE_SOURCE || request.version !== this.version) return null;
    const count = Math.min(
      request.range.count,
      PROCESS_TABLE_WINDOW_LIMIT,
      Math.max(0, this.rows.length - request.range.start),
    );
    const rows = this.rows.slice(request.range.start, request.range.start + count);
    this.generated += rows.length;
    return {
      source: PROCESS_TABLE_SOURCE,
      version: this.version,
      request: request.id,
      range: request.range,
      rows,
    };
  }
}

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
    await this.send({
      Length: { source: IMAGE_DETAIL_SOURCE, version: this.version, rows: this.rows.length },
    });
    return this.rows.length;
  }

  answer(value: unknown): RowWindow | null {
    const request = rowRequest(value);
    if (!request) return null;
    if (request.source !== IMAGE_DETAIL_SOURCE || request.version !== this.version) return null;
    const count = Math.min(
      request.range.count,
      IMAGE_DETAIL_WINDOW_LIMIT,
      Math.max(0, this.rows.length - request.range.start),
    );
    const rows = this.rows.slice(request.range.start, request.range.start + count);
    this.generated += rows.length;
    return {
      source: IMAGE_DETAIL_SOURCE,
      version: this.version,
      request: request.id,
      range: request.range,
      rows,
    };
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
    await this.send({
      Length: { source: CONTAINER_DETAIL_SOURCE, version: this.version, rows: this.rows.length },
    });
    return this.rows.length;
  }

  answer(value: unknown): RowWindow | null {
    const request = rowRequest(value);
    if (!request) return null;
    if (request.source !== CONTAINER_DETAIL_SOURCE || request.version !== this.version) return null;
    const count = Math.min(
      request.range.count,
      CONTAINER_DETAIL_WINDOW_LIMIT,
      Math.max(0, this.rows.length - request.range.start),
    );
    return {
      source: CONTAINER_DETAIL_SOURCE,
      version: this.version,
      request: request.id,
      range: request.range,
      rows: this.rows.slice(request.range.start, request.range.start + count),
    };
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
    const result = details?.result;
    const outcome =
      result?.kind === 'code'
        ? `Exited with code ${result.value}`
        : result?.kind === 'signal'
          ? `Stopped by signal ${result.value}`
          : result?.kind === 'fault'
            ? `Runtime fault (${result.value.reason.replaceAll('_', ' ')}, status ${result.value.status})`
            : details && !details.running
              ? `Exited with code ${details.exit_code}`
              : null;
    const instant = (value: number | null | undefined) =>
      value == null ? null : new Date(value).toISOString();
    const values: Array<[string, unknown]> = [
      ['Execution ID', details?.id],
      ['Container ID', details?.container_id],
      ['State', details && 'running' in details ? (details.running ? 'running' : 'exited') : null],
      ['Result', outcome],
      ['Created', instant(details?.created_at_ms)],
      ['Started', instant(details?.started_at_ms)],
      ['Finished', instant(details?.finished_at_ms)],
      ['Process ID', details && details.pid > 0 ? String(details.pid) : null],
      ['Command', details?.command?.join(' ')],
      ['User', details && 'user' in details ? details.user || 'default user' : null],
    ];
    this.rows = detailRows(values);
    this.version += 1;
    await this.send({
      Length: { source: EXECUTION_DETAIL_SOURCE, version: this.version, rows: this.rows.length },
    });
    return this.rows.length;
  }

  answer(value: unknown): RowWindow | null {
    const request = rowRequest(value);
    if (!request) return null;
    if (request.source !== EXECUTION_DETAIL_SOURCE || request.version !== this.version) return null;
    const count = Math.min(
      request.range.count,
      EXECUTION_DETAIL_WINDOW_LIMIT,
      Math.max(0, this.rows.length - request.range.start),
    );
    return {
      source: EXECUTION_DETAIL_SOURCE,
      version: this.version,
      request: request.id,
      range: request.range,
      rows: this.rows.slice(request.range.start, request.range.start + count),
    };
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
      ['Connected containers', details?.endpoints?.containers.join(', ')],
      ['Endpoint membership truncated', details?.endpoints?.truncated],
    ]);
    this.version += 1;
    await this.send({
      Length: { source: NETWORK_DETAIL_SOURCE, version: this.version, rows: this.rows.length },
    });
    return this.rows.length;
  }

  answer(value: unknown): RowWindow | null {
    const request = rowRequest(value);
    if (!request) return null;
    if (request.source !== NETWORK_DETAIL_SOURCE || request.version !== this.version) return null;
    const count = Math.min(
      request.range.count,
      NETWORK_DETAIL_WINDOW_LIMIT,
      Math.max(0, this.rows.length - request.range.start),
    );
    return {
      source: NETWORK_DETAIL_SOURCE,
      version: this.version,
      request: request.id,
      range: request.range,
      rows: this.rows.slice(request.range.start, request.range.start + count),
    };
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
    await this.send({
      Length: { source: VOLUME_DETAIL_SOURCE, version: this.version, rows: this.rows.length },
    });
    return this.rows.length;
  }

  answer(value: unknown): RowWindow | null {
    const request = rowRequest(value);
    if (!request) return null;
    if (request.source !== VOLUME_DETAIL_SOURCE || request.version !== this.version) return null;
    const count = Math.min(
      request.range.count,
      VOLUME_DETAIL_WINDOW_LIMIT,
      Math.max(0, this.rows.length - request.range.start),
    );
    return {
      source: VOLUME_DETAIL_SOURCE,
      version: this.version,
      request: request.id,
      range: request.range,
      rows: this.rows.slice(request.range.start, request.range.start + count),
    };
  }
}

export function bytes(value: unknown): string {
  const amount = Number(value ?? 0);
  if (!Number.isFinite(amount) || amount < 1) return '0 B';
  const units = ['B', 'KiB', 'MiB', 'GiB', 'TiB'];
  const rank = Math.min(Math.floor(Math.log(amount) / Math.log(1024)), units.length - 1);
  return `${(amount / 1024 ** rank).toFixed(rank === 0 ? 0 : 1)} ${units[rank]}`;
}

export function logText(
  log: ContainerOutput | Uint8Array | readonly number[] | string | null | undefined,
): string {
  if (typeof log === 'string') return log;
  if (log instanceof Uint8Array || Array.isArray(log))
    return new TextDecoder().decode(Uint8Array.from(log));
  if (log && typeof log === 'object' && 'stdout' in log) {
    return [log.stdout, log.stderr]
      .filter(Boolean)
      .map((stream) => logText(stream))
      .join('\n');
  }
  return '';
}

/** Turns the protocol's title + matrix process list into labelled cells. */
export function processRows(list: ProcessList, container: string): ProcessRecord[] {
  const titles = Array.isArray(list?.titles) ? list.titles.map(String) : [];
  const rows = Array.isArray(list?.processes) ? list.processes : [];
  return rows.map((cells) => ({
    container,
    cells: Object.fromEntries(titles.map((title, index) => [title, String(cells?.[index] ?? '')])),
    values: Array.isArray(cells) ? cells.map(String) : [],
  }));
}

/** Maximum records mounted into a native tree at once. */
export const RECORD_LIMIT = 200;
export const LOG_LIMIT = 400;

const CONTAINER_NAME = /^[A-Za-z0-9][A-Za-z0-9_.-]{0,127}$/;

/** The native container-name grammar, expressed as a user-facing validation result. */
export function containerNameError(name) {
  return typeof name === 'string' && CONTAINER_NAME.test(name)
    ? ''
    : 'Container name must contain 1–128 ASCII letters, digits, underscores, periods, or hyphens and start with a letter or digit.';
}

/** A bounded view plus the number honestly omitted. */
export function bounded(records, limit = RECORD_LIMIT) {
  const all = Array.isArray(records) ? records : [];
  return { records: all.slice(0, limit), omitted: Math.max(0, all.length - limit) };
}

export function endpointAliases(value) {
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

export function immutableContainerId(value) {
  return /^[0-9a-f]{64}$/.test(value);
}

export function boundedMessage(value, limit = 512) {
  const message = value?.message ?? String(value ?? '');
  return message.length <= limit ? message : `${message.slice(0, limit)}…`;
}

export function shortId(value) {
  const id = String(value ?? '—');
  return id.length > 12 ? id.slice(0, 12) : id;
}

/** Stable daemon reference: prefer an opaque ID, then the human name. */
export function resourceReference(resource) {
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
  constructor(send = async () => {}) {
    this.send = send;
    this.version = 0;
    this.rows = [];
    this.generated = 0;
  }

  async replace(details) {
    const values = [
      ['ID', details?.id],
      ['References', details?.references?.join(', ')],
      ['Created', details?.created],
      ['Size', Number.isFinite(details?.size) ? bytes(details.size) : null],
      ['Operating system', details?.os],
      ['Architecture', details?.architecture],
      ['Entrypoint', details?.entrypoint?.join(' ')],
      ['Command', details?.command?.join(' ')],
      ['Working directory', details?.working_directory],
      ['User', details && 'user' in details ? details.user || 'default user' : null],
    ];
    this.rows = values
      .filter(([, value]) => value !== null && value !== undefined && String(value).length > 0)
      .slice(0, IMAGE_DETAIL_LIMIT)
      .map(([key, value], index) => ({ id: index + 1, cells: [{ Text: key }, { Code: String(value) }] }));
    this.version += 1;
    await this.send({ Length: { source: IMAGE_DETAIL_SOURCE, version: this.version, rows: this.rows.length } });
    return this.rows.length;
  }

  answer(request) {
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
  constructor(send = async () => {}) {
    this.send = send;
    this.version = 0;
    this.rows = [];
  }

  async replace(details) {
    const values = [
      ['Immutable ID', details?.id],
      ['Name', details?.name],
      ['State', details?.state],
      ['Image', details?.image],
      ['Created', Number.isFinite(details?.created) ? String(details.created) : null],
    ];
    this.rows = values.filter(([, value]) => value !== null && value !== undefined && String(value).length > 0)
      .map(([key, value], index) => ({ id: index + 1, cells: [{ Text: key }, { Code: String(value) }] }));
    this.version += 1;
    await this.send({ Length: { source: CONTAINER_DETAIL_SOURCE, version: this.version, rows: this.rows.length } });
    return this.rows.length;
  }

  answer(request) {
    if (request.source !== CONTAINER_DETAIL_SOURCE || request.version !== this.version) return null;
    const count = Math.min(request.range.count, CONTAINER_DETAIL_WINDOW_LIMIT,
      Math.max(0, this.rows.length - request.range.start));
    return { source: CONTAINER_DETAIL_SOURCE, version: this.version, request: request.id,
      range: request.range, rows: this.rows.slice(request.range.start, request.range.start + count) };
  }
}

/** Windowed rows from the typed ExecutionSummary contract. */
export class ExecutionDetailsSource {
  constructor(send = async () => {}) {
    this.send = send;
    this.version = 0;
    this.rows = [];
  }

  async replace(details) {
    const values = [
      ['Execution ID', details?.id],
      ['Container ID', details?.container_id],
      ['State', details ? details.running ? 'running' : 'exited' : null],
      ['Exit code', details && !details.running ? String(details.exit_code) : null],
      ['Process ID', details?.pid > 0 ? String(details.pid) : null],
      ['Command', details?.command?.join(' ')],
      ['User', details?.user || 'default user'],
    ];
    this.rows = values.filter(([, value]) => value !== null && value !== undefined && String(value).length > 0)
      .map(([key, value], index) => ({ id: index + 1, cells: [{ Text: key }, { Code: String(value) }] }));
    this.version += 1;
    await this.send({ Length: { source: EXECUTION_DETAIL_SOURCE, version: this.version, rows: this.rows.length } });
    return this.rows.length;
  }

  answer(request) {
    if (request.source !== EXECUTION_DETAIL_SOURCE || request.version !== this.version) return null;
    const count = Math.min(request.range.count, EXECUTION_DETAIL_WINDOW_LIMIT,
      Math.max(0, this.rows.length - request.range.start));
    return { source: EXECUTION_DETAIL_SOURCE, version: this.version, request: request.id,
      range: request.range, rows: this.rows.slice(request.range.start, request.range.start + count) };
  }
}

/** Windowed rows from the typed NetworkSummary contract. */
export class NetworkDetailsSource {
  constructor(send = async () => {}) {
    this.send = send;
    this.version = 0;
    this.rows = [];
  }

  async replace(details) {
    this.rows = [
      ['Network ID', details?.id],
      ['Name', details?.name],
      ['Driver', details?.driver],
      ['Scope', details?.scope],
    ].filter(([, value]) => value !== null && value !== undefined && String(value).length > 0)
      .map(([key, value], index) => ({ id: index + 1, cells: [{ Text: key }, { Code: String(value) }] }));
    this.version += 1;
    await this.send({ Length: { source: NETWORK_DETAIL_SOURCE, version: this.version, rows: this.rows.length } });
    return this.rows.length;
  }

  answer(request) {
    if (request.source !== NETWORK_DETAIL_SOURCE || request.version !== this.version) return null;
    const count = Math.min(request.range.count, NETWORK_DETAIL_WINDOW_LIMIT,
      Math.max(0, this.rows.length - request.range.start));
    return { source: NETWORK_DETAIL_SOURCE, version: this.version, request: request.id,
      range: request.range, rows: this.rows.slice(request.range.start, request.range.start + count) };
  }
}

/** Windowed rows from the deliberately small typed VolumeSummary contract. */
export class VolumeDetailsSource {
  constructor(send = async () => {}) {
    this.send = send;
    this.version = 0;
    this.rows = [];
  }

  async replace(details) {
    this.rows = [
      ['Name', details?.name],
      ['Driver', details?.driver],
    ].filter(([, value]) => value !== null && value !== undefined && String(value).length > 0)
      .map(([key, value], index) => ({ id: index + 1, cells: [{ Text: key }, { Code: String(value) }] }));
    this.version += 1;
    await this.send({ Length: { source: VOLUME_DETAIL_SOURCE, version: this.version, rows: this.rows.length } });
    return this.rows.length;
  }

  answer(request) {
    if (request.source !== VOLUME_DETAIL_SOURCE || request.version !== this.version) return null;
    const count = Math.min(request.range.count, VOLUME_DETAIL_WINDOW_LIMIT,
      Math.max(0, this.rows.length - request.range.start));
    return { source: VOLUME_DETAIL_SOURCE, version: this.version, request: request.id,
      range: request.range, rows: this.rows.slice(request.range.start, request.range.start + count) };
  }
}

export function bytes(value) {
  const amount = Number(value ?? 0);
  if (!Number.isFinite(amount) || amount < 1) return '0 B';
  const units = ['B', 'KiB', 'MiB', 'GiB', 'TiB'];
  const rank = Math.min(Math.floor(Math.log(amount) / Math.log(1024)), units.length - 1);
  return `${(amount / 1024 ** rank).toFixed(rank === 0 ? 0 : 1)} ${units[rank]}`;
}

export function logText(log) {
  if (typeof log === 'string') return log;
  if (log instanceof Uint8Array || Array.isArray(log)) return new TextDecoder().decode(Uint8Array.from(log));
  if (log && typeof log === 'object') {
    return [log.stdout, log.stderr].filter(Boolean).map(logText).join('\n');
  }
  return '';
}

/** Turns the protocol's title + matrix process list into labelled cells. */
export function processRows(list, container) {
  const titles = Array.isArray(list?.titles) ? list.titles.map(String) : [];
  const rows = Array.isArray(list?.processes) ? list.processes : [];
  return rows.map((cells) => ({
    container,
    cells: Object.fromEntries(titles.map((title, index) => [title, String(cells?.[index] ?? '')])),
    values: Array.isArray(cells) ? cells.map(String) : [],
  }));
}

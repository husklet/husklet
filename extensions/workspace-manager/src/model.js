/** Maximum records mounted into a native tree at once. */
export const RECORD_LIMIT = 200;
export const LOG_LIMIT = 400;

/** A bounded view plus the number honestly omitted. */
export function bounded(records, limit = RECORD_LIMIT) {
  const all = Array.isArray(records) ? records : [];
  return { records: all.slice(0, limit), omitted: Math.max(0, all.length - limit) };
}

export function shortId(value) {
  const id = String(value ?? '—');
  return id.length > 12 ? id.slice(0, 12) : id;
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

const SECRET = /(authorization|cookie|password|secret|token|credential|private.?key)/i;
const STRING_LIMIT = 8192;
const ARRAY_LIMIT = 200;
export const OUTPUT_LIMIT = 64 * 1024;
export const INVENTORY_ITEMS_LIMIT = 200;
const LOG_STREAM_LIMIT = 7_500;
export const FILE_BYTES_LIMIT = 12_000;

function safe(value, key = '', depth = 0) {
  if (SECRET.test(key)) return '[redacted]';
  if (depth >= 8) return '[depth limit]';
  if (typeof value === 'string') {
    return value.length > STRING_LIMIT ? `${value.slice(0, STRING_LIMIT - 20)}… [truncated]` : value;
  }
  if (Array.isArray(value)) return value.slice(0, ARRAY_LIMIT).map((item) => safe(item, '', depth + 1));
  if (value && typeof value === 'object') {
    return Object.fromEntries(Object.entries(value).slice(0, ARRAY_LIMIT).map(([name, item]) => [name, safe(item, name, depth + 1)]));
  }
  return value;
}

export function result(value) {
  let text = JSON.stringify(safe(value));
  const encoded = new TextEncoder().encode(text);
  if (encoded.byteLength > OUTPUT_LIMIT) {
    text = `${new TextDecoder().decode(encoded.slice(0, OUTPUT_LIMIT - 40))}\n… [output truncated]`;
  }
  return { content: [{ type: 'text', text }] };
}

/** Preserve log completeness metadata when MCP applies its own output bound. */
export function logResult(value) {
  const stdout = Array.isArray(value?.stdout) ? value.stdout.slice(0, LOG_STREAM_LIMIT) : [];
  const stderr = Array.isArray(value?.stderr) ? value.stderr.slice(0, LOG_STREAM_LIMIT) : [];
  const stdoutTruncated = value?.stdout_truncated === true || stdout.length < (value?.stdout?.length ?? 0);
  const stderrTruncated = value?.stderr_truncated === true || stderr.length < (value?.stderr?.length ?? 0);
  return { content: [{ type: 'text', text: JSON.stringify({
    stdout,
    stderr,
    truncated: value?.truncated === true || stdoutTruncated || stderrTruncated,
    stdout_truncated: stdoutTruncated,
    stderr_truncated: stderrTruncated,
    eof: value?.eof === true,
  }) }] };
}

/** Legacy whole-file result: exact or rejected, never silently shortened. */
export function fileResult(value) {
  const length = typeof value === 'string' ? new TextEncoder().encode(value).byteLength : value?.length;
  if ((!Array.isArray(value) && typeof value !== 'string') || length > FILE_BYTES_LIMIT) {
    throw new RangeError(`file exceeds the ${FILE_BYTES_LIMIT}-byte MCP whole-read limit; use husklet_file_read_range`);
  }
  return { content: [{ type: 'text', text: JSON.stringify(value) }] };
}

/** A single explicitly incomplete file observation. */
export function fileRangeResult(value) {
  return { content: [{ type: 'text', text: JSON.stringify(value) }] };
}

function strictInventory(value, key = '', depth = 0) {
  if (SECRET.test(key)) return '[redacted]';
  if (depth >= 8) throw new RangeError('inventory exceeds the MCP depth limit');
  if (typeof value === 'string') {
    if (value.length > STRING_LIMIT) throw new RangeError('inventory string exceeds the MCP string limit');
    return value;
  }
  if (Array.isArray(value)) {
    if (value.length > INVENTORY_ITEMS_LIMIT) throw new RangeError('inventory nested array exceeds the MCP item limit');
    if (key === 'environment') return value.map((item) => {
      if (!Array.isArray(item) || item.length !== 2) throw new TypeError('environment entry must be a name/value pair');
      return [strictInventory(item[0], '', depth + 1), SECRET.test(String(item[0])) ? '[redacted]' : strictInventory(item[1], '', depth + 1)];
    });
    return value.map((item) => strictInventory(item, '', depth + 1));
  }
  if (value && typeof value === 'object') {
    const entries = Object.entries(value);
    if (entries.length > INVENTORY_ITEMS_LIMIT) throw new RangeError('inventory object exceeds the MCP field limit');
    return Object.fromEntries(entries.map(([name, item]) => [name, strictInventory(item, name, depth + 1)]));
  }
  return value;
}

/** Serialize inventories as valid JSON without unmarked local omission. */
export function inventoryResult(value, field) {
  let bounded = value;
  if (field != null) {
    const items = value?.[field];
    if (!Array.isArray(items)) throw new TypeError(`inventory ${field} must be an array`);
    const omitted = items.length > INVENTORY_ITEMS_LIMIT;
    bounded = { ...value, [field]: items.slice(0, INVENTORY_ITEMS_LIMIT), truncated: value?.truncated === true || omitted };
  } else if (Array.isArray(value) && value.length > INVENTORY_ITEMS_LIMIT) {
    throw new RangeError(`inventory exceeds the ${INVENTORY_ITEMS_LIMIT}-item MCP limit and has no truncation metadata`);
  }
  const text = JSON.stringify(strictInventory(bounded));
  if (new TextEncoder().encode(text).byteLength > OUTPUT_LIMIT) {
    throw new RangeError(`inventory exceeds the ${OUTPUT_LIMIT}-byte MCP output limit`);
  }
  return { content: [{ type: 'text', text }] };
}

/** Serialize one complete detail object, redacted but never locally clipped. */
export function detailResult(value) {
  const text = JSON.stringify(strictInventory(value));
  if (new TextEncoder().encode(text).byteLength > OUTPUT_LIMIT) {
    throw new RangeError(`detail exceeds the ${OUTPUT_LIMIT}-byte MCP output limit`);
  }
  return { content: [{ type: 'text', text }] };
}

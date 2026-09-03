const SECRET = /(authorization|cookie|password|secret|token|credential|private.?key)/i;
const STRING_LIMIT = 8192;
const ARRAY_LIMIT = 200;
export const OUTPUT_LIMIT = 64 * 1024;
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

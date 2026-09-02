const SECRET = /(authorization|cookie|password|secret|token|credential|private.?key)/i;
const STRING_LIMIT = 8192;
const ARRAY_LIMIT = 200;
export const OUTPUT_LIMIT = 64 * 1024;

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

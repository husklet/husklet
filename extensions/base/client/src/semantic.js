const XML_LIMIT = 64 * 1024;
const NODE_LIMIT = 256;
const DEPTH_LIMIT = 32;
const TEXT_LIMIT = 256;
const SECRET = /(password|secret|token|credential|private.?key)/i;
const bytes = (text) => new TextEncoder().encode(text).byteLength;
const boundedText = (value, limit = TEXT_LIMIT) => {
  const characters = Array.from(String(value));
  return { value: characters.slice(0, limit).join(''), truncated: characters.length > limit };
};
const escapeXml = (value) => Array.from(String(value), (character) => {
  const point = character.codePointAt(0);
  return point <= 0x1f && point !== 0x09 && point !== 0x0a && point !== 0x0d
    || (point >= 0x7f && point <= 0x9f) || (point >= 0xd800 && point <= 0xdfff) ? '\uFFFD' : character;
}).join('').replaceAll('&', '&amp;').replaceAll('<', '&lt;').replaceAll('>', '&gt;')
  .replaceAll('"', '&quot;').replaceAll("'", '&apos;')
  .replaceAll('\t', '&#x9;').replaceAll('\n', '&#xA;').replaceAll('\r', '&#xD;');

/** Deterministic bounded XML projection of one validated semantic snapshot. */
export function semanticXml(tree) {
  if (!Number.isSafeInteger(tree?.generation) || tree.generation < 0
    || !Number.isSafeInteger(tree?.revision) || tree.revision < 0) {
    throw new TypeError('semantic tree requires nonnegative safe integer generation and revision');
  }
  let output = ''; let used = 0; let nodes = 0; let cut = false;
  const append = (text, reserve = 0) => {
    const size = bytes(text);
    if (used + size + reserve > XML_LIMIT) { cut = true; return false; }
    output += text; used += size; return true;
  };
  const attr = (value) => escapeXml(boundedText(value).value);
  const writeNode = (entry, depth, reserve) => {
    if (!entry || typeof entry !== 'object' || nodes >= NODE_LIMIT || depth >= DEPTH_LIMIT) { cut = true; return; }
    nodes += 1;
    const values = Array.isArray(entry.actions) ? entry.actions : [];
    const actions = values.slice(0, 16).map(attr).join(',');
    const id = boundedText(entry.id ?? ''); const role = boundedText(entry.role ?? '');
    const close = '</node>';
    if (!append(`<node id="${escapeXml(id.value)}" role="${escapeXml(role.value)}" disabled="${entry.disabled === true}" destructive="${entry.destructive === true}" actions="${actions}">`, reserve + bytes(close) + 14)) return;
    if (entry.label != null) { const label = boundedText(entry.label); append(`<label${label.truncated ? ' truncated="true"' : ''}>${escapeXml(label.value)}</label>`, reserve + bytes(close) + 14); }
    if (entry.value != null) {
      const value = SECRET.test(`${entry.role ?? ''} ${entry.label ?? ''}`) ? '[redacted]' : entry.value;
      const field = boundedText(value);
      append(`<value${field.truncated ? ' truncated="true"' : ''}>${escapeXml(field.value)}</value>`, reserve + bytes(close) + 14);
    }
    for (const child of Array.isArray(entry.children) ? entry.children : []) { if (cut) break; writeNode(child, depth + 1, reserve + bytes(close)); }
    if (cut) append('<truncated/>', reserve + bytes(close));
    append(close);
  };
  const close = '</pane>';
  append(`<pane slot="${attr(tree?.slot ?? '')}" generation="${tree.generation}" revision="${tree.revision}" truncated="${tree?.truncated === true}">`, bytes(close) + 14);
  writeNode(tree?.root, 0, bytes(close));
  if (cut) append('<truncated/>', bytes(close));
  append(close);
  return output;
}

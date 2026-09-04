import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
const repository = path.resolve(here, '../../..');
const schemaPath = path.join(repository, 'src/workspaces/hl-extension/protocol/v1.json');
const schema = JSON.parse(fs.readFileSync(schemaPath, 'utf8'));
const output = path.resolve(here, '../src');

function walk(node, visit) {
  if (!node || typeof node !== 'object') return;
  visit(node);
  for (const value of Object.values(node)) walk(value, visit);
}
walk(schema, (node) => {
  assert.notEqual(node.kind, 'external_ref', `unresolved external protocol schema ${node.package}::${node.name}`);
  if (node.kind === 'ref') assert(schema.definitions[node.name], `unresolved protocol reference ${node.name}`);
});
for (const [name, definition] of Object.entries(schema.definitions)) {
  assert(!(definition.kind === 'ref' && definition.name === name), `non-progressing self reference ${name}`);
}

const stable = (value) => JSON.stringify(value, Object.keys(value).sort());
const requestVariants = schema.roots.request.variants.map(({ name }) => name);
const replyVariants = new Set(schema.roots.reply.variants.map(({ name }) => name));
assert(Array.isArray(schema.request_to_reply), 'Rust protocol schema lacks request_to_reply');
assert.deepEqual(schema.request_to_reply.map(({ request }) => request), requestVariants, 'request_to_reply must cover Request exactly in declaration order');
const expectedReplies = Object.fromEntries(schema.request_to_reply.map(({ request, reply }) => [request, reply]));
for (const [call, reply] of Object.entries(expectedReplies)) assert(replyVariants.has(reply), `${call} expects absent reply ${reply}`);
assert(Array.isArray(schema.request_to_capability), 'Rust protocol schema lacks request_to_capability');
assert.deepEqual(schema.request_to_capability.map(({ request }) => request), requestVariants, 'request_to_capability must cover Request exactly in declaration order');
const requestCapabilities = Object.fromEntries(schema.request_to_capability.map(({ request, capability }) => [request, capability]));
const capabilities = new Set(schema.capabilities.map(({ wire }) => wire));
for (const [call, capability] of Object.entries(requestCapabilities)) {
  if (capability === null) assert(['event_subscribe', 'event_unsubscribe'].includes(call), `${call} has no fixed capability`);
  else assert(capabilities.has(capability), `${call} requires absent capability ${capability}`);
}
const runtime = `// Generated from Rust hl-extension protocol/v1.json. Do not edit.
export const PROTOCOL_SPECIFICATION_VERSION = ${schema.specification_version};
export const PROTOCOL_VERSION = ${schema.protocol_version};
export const PROTOCOL_BOUNDS = Object.freeze(${JSON.stringify(schema.bounds, null, 2)});
export const PROTOCOL_CAPABILITIES = Object.freeze(${JSON.stringify(schema.capabilities, null, 2)});
export const PROTOCOL_TOPICS = Object.freeze(${JSON.stringify(schema.topics, null, 2)});
export const PROTOCOL_REPLIES = Object.freeze(${JSON.stringify(expectedReplies, null, 2)});
export const PROTOCOL_REQUEST_CAPABILITIES = Object.freeze(${JSON.stringify(requestCapabilities, null, 2)});
const definitions = ${JSON.stringify(schema.definitions, null, 2)};
const roots = ${JSON.stringify(schema.roots, null, 2)};

function fail(path, expected) { throw new TypeError(\`\${path} must be \${expected}\`); }
function validate(schema, value, path) {
  switch (schema.kind) {
    case 'unit': if (value !== undefined && value !== null) fail(path, 'absent'); return;
    case 'string': if (typeof value !== 'string') fail(path, 'a string'); return;
    case 'boolean': if (typeof value !== 'boolean') fail(path, 'a boolean'); return;
    case 'integer': if (!Number.isSafeInteger(value) || value < schema.minimum || value > schema.maximum) fail(path, \`an integer from \${schema.minimum} through \${schema.maximum}\`); return;
    case 'float': if (typeof value !== 'number' || !Number.isFinite(value)) fail(path, 'a finite number'); return;
    case 'optional': if (value !== null && value !== undefined) validate(schema.of, value, path); return;
    case 'newtype': return validate(schema.of, value, path);
    case 'array': if (!Array.isArray(value)) fail(path, 'an array'); value.forEach((entry, index) => validate(schema.of, entry, \`\${path}[\${index}]\`)); return;
    case 'tuple': {
      const fields = schema.items ?? schema.fields?.map((field) => field.schema) ?? [];
      if (fields.length === 1) return validate(fields[0], value, path);
      if (!Array.isArray(value) || value.length !== fields.length) fail(path, \`a \${fields.length}-item tuple\`);
      fields.forEach((field, index) => validate(field, value[index], \`\${path}[\${index}]\`)); return;
    }
    case 'map': if (!value || typeof value !== 'object' || Array.isArray(value)) fail(path, 'an object map'); for (const [key, entry] of Object.entries(value)) { validate(schema.key, key, path); validate(schema.value, entry, \`\${path}.\${key}\`); } return;
    case 'ref': return validate(definitions[schema.name], value, path);
    case 'struct':
      if (!value || typeof value !== 'object' || Array.isArray(value)) fail(path, 'an object');
      for (const field of schema.fields) {
        if (!field.optional && !(field.name in value)) fail(\`\${path}.\${field.name}\`, 'present');
        if (field.name in value) validate(field.schema, value[field.name], \`\${path}.\${field.name}\`);
      }
      if (schema.serde?.deny_unknown_fields) for (const key of Object.keys(value)) if (!schema.fields.some((field) => field.name === key)) fail(\`\${path}.\${key}\`, 'a declared field');
      return;
    case 'enum': return validateEnum(schema, value, path);
    default: throw new TypeError(\`unsupported protocol schema kind \${schema.kind} at \${path}\`);
  }
}
function validateEnum(schema, value, path) {
  const tag = schema.serde?.tag;
  const content = schema.serde?.content;
  if (tag) {
    if (!value || typeof value !== 'object' || Array.isArray(value) || typeof value[tag] !== 'string') fail(path, \`an object tagged by \${tag}\`);
    const variant = schema.variants.find((entry) => entry.name === value[tag]);
    if (!variant) fail(\`\${path}.\${tag}\`, 'a known variant');
    if (content) return validate(variant.payload, value[content], \`\${path}.\${content}\`);
    if (variant.payload.kind === 'unit') return;
    const body = { ...value }; delete body[tag]; return validate(variant.payload, body, path);
  }
  if (typeof value === 'string') {
    if (!schema.variants.some((entry) => entry.name === value && entry.payload.kind === 'unit')) fail(path, 'a known unit variant');
    return;
  }
  if (!value || typeof value !== 'object' || Array.isArray(value) || Object.keys(value).length !== 1) fail(path, 'an externally tagged variant');
  const [name] = Object.keys(value); const variant = schema.variants.find((entry) => entry.name === name);
  if (!variant) fail(path, 'a known variant'); validate(variant.payload, value[name], \`\${path}.\${name}\`);
}
export function validateRequest(value) { validate(roots.request, value, 'request'); return value; }
export function validateReply(value) { validate(roots.reply, value, 'reply'); return value; }
export function validateReplyFor(call, value) {
  validateReply(value);
  const expected = PROTOCOL_REPLIES[call];
  if (expected === undefined) fail('call', 'a known operation');
  if (value.reply !== expected) fail('reply.reply', expected);
  return value;
}
export function validateFailure(value) { validate(roots.failure, value, 'failure'); return value; }
export function validateSnapshot(value) { validate(roots.snapshot, value, 'snapshot'); return value; }
export function validateUiEvent(value) { validate(roots.uievent, value, 'ui event'); return value; }
export function encodeRequest(call, payload) {
  return validateRequest(payload === undefined ? { call } : { call, with: payload });
}
`;

function type(schemaNode) {
  switch (schemaNode.kind) {
    case 'unit': return 'undefined';
    case 'string': return 'string';
    case 'boolean': return 'boolean';
    case 'integer': case 'float': return 'number';
    case 'optional': return `${type(schemaNode.of)} | null`;
    case 'newtype': return type(schemaNode.of);
    case 'array': return `Array<${type(schemaNode.of)}>`;
    case 'tuple': {
      const fields = schemaNode.items ?? schemaNode.fields?.map((field) => field.schema) ?? [];
      return fields.length === 1 ? type(fields[0]) : `[${fields.map(type).join(', ')}]`;
    }
    case 'map': return `Record<string, ${type(schemaNode.value)}>`;
    case 'ref': return schemaNode.name;
    case 'struct': return `{ ${schemaNode.fields.map((field) => `${JSON.stringify(field.name)}${field.optional ? '?' : ''}: ${type(field.schema)}`).join('; ')} }`;
    case 'enum': return enumType(schemaNode);
    default: throw new Error(`unsupported TypeScript schema kind ${schemaNode.kind}`);
  }
}
function enumType(node) {
  const { tag, content } = node.serde ?? {};
  return node.variants.map((variant) => {
    const payload = type(variant.payload);
    if (tag && content) return variant.payload.kind === 'unit' ? `{ ${tag}: ${JSON.stringify(variant.name)} }` : `{ ${tag}: ${JSON.stringify(variant.name)}; ${content}: ${payload} }`;
    if (tag) return variant.payload.kind === 'unit' ? `{ ${tag}: ${JSON.stringify(variant.name)} }` : `{ ${tag}: ${JSON.stringify(variant.name)} } & ${payload}`;
    return variant.payload.kind === 'unit' ? JSON.stringify(variant.name) : `{ ${JSON.stringify(variant.name)}: ${payload} }`;
  }).join(' | ');
}
const declarations = `// Generated from Rust hl-extension protocol/v1.json. Do not edit.
export const PROTOCOL_SPECIFICATION_VERSION: ${schema.specification_version};
export const PROTOCOL_VERSION: ${schema.protocol_version};
export const PROTOCOL_BOUNDS: Readonly<${type({ kind: 'struct', fields: Object.entries(schema.bounds).map(([name]) => ({name, optional:false, schema:{kind:'integer',signed:false}})) })}>;
export type ExtensionCapability = ${schema.capabilities.map(({wire}) => JSON.stringify(wire)).join(' | ')};
${Object.entries(schema.definitions).map(([name, definition]) => `export type ${name} = ${type(definition)};`).join('\n')}
export type WireRequest = ${type(schema.roots.request)};
export type WireReply = ${type(schema.roots.reply)};
export type WireCall = WireRequest['call'];
export type WireRequestFor<C extends WireCall> = Extract<WireRequest, { call: C }>;
export type WireRequestParameters<C extends WireCall> = WireRequestFor<C> extends { with: infer P } ? P : undefined;
export interface WireReplyByCall {
${schema.request_to_reply.map(({ request, reply }) => `  ${JSON.stringify(request)}: Extract<WireReply, { reply: ${JSON.stringify(reply)} }>;`).join('\n')}
}
export type WireReplyFor<C extends WireCall> = WireReplyByCall[C];
export type WireFailure = ${type(schema.roots.failure)};
export type WireSnapshot = ${type(schema.roots.snapshot)};
export type WireUiEvent = ${type(schema.roots.uievent)};
export const PROTOCOL_REPLIES: Readonly<Record<WireRequest['call'], WireReply['reply']>>;
export const PROTOCOL_REQUEST_CAPABILITIES: Readonly<Record<WireRequest['call'], ExtensionCapability | null>>;
export function validateRequest(value: unknown): WireRequest;
export function validateReply(value: unknown): WireReply;
export function validateReplyFor(call: WireRequest['call'], value: unknown): WireReply;
export function validateFailure(value: unknown): WireFailure;
export function validateSnapshot(value: unknown): WireSnapshot;
export function validateUiEvent(value: unknown): WireUiEvent;
export function encodeRequest(call: WireRequest['call'], payload?: unknown): WireRequest;
`;
const files = [['generated-protocol.js', runtime], ['generated-protocol.d.ts', declarations]];
for (const [name, contents] of files) {
  const target = path.join(output, name);
  if (process.argv.includes('--write')) fs.writeFileSync(target, contents);
  else assert.equal(fs.readFileSync(target, 'utf8'), contents, `${name} is stale; run npm run protocol:generate`);
}

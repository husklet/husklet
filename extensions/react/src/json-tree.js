import React from 'react';
import { Badge, Button, Column, EmptyState, InlineMessage, Row, Scroll, Search, Text } from './components.js';

const { createElement: h, useMemo, useState } = React;

export const JSON_TREE_DEFAULTS = Object.freeze({ maxDepth: 12, maxNodes: 500, maxStringLength: 512 });

function limitedInteger(value, fallback, ceiling) {
  return Number.isInteger(value) && value >= 1 ? Math.min(value, ceiling) : fallback;
}

function kind(value) {
  if (value === null) return 'null';
  if (Array.isArray(value)) return 'array';
  return typeof value === 'object' ? 'object' : typeof value;
}

function childrenOf(value) {
  if (Array.isArray(value)) return value.map((child, index) => [String(index), child]);
  if (value !== null && typeof value === 'object') return Object.keys(value).map((key) => [key, value[key]]);
  return [];
}

function segment(parent, key, array) {
  if (array) return `${parent}[${key}]`;
  return /^[A-Za-z_$][\w$]*$/.test(key) ? `${parent}.${key}` : `${parent}[${JSON.stringify(key)}]`;
}

function scalar(value, type, limit) {
  if (type === 'string') {
    if (value.length <= limit) return { display: JSON.stringify(value), truncated: false };
    return { display: `${JSON.stringify(value.slice(0, limit))}… (+${value.length - limit} characters)`, truncated: true };
  }
  if (type === 'undefined') return { display: 'undefined', truncated: false };
  if (type === 'bigint') return { display: `${value}n`, truncated: false };
  if (type === 'number' && !Number.isFinite(value)) return { display: String(value), truncated: false };
  if (type === 'function') return { display: '[Function]', truncated: false };
  if (type === 'symbol') return { display: String(value), truncated: false };
  return { display: JSON.stringify(value), truncated: false };
}

/** Converts arbitrary JSON-like data to a finite, cycle-safe row model. */
export function inspectJson(value, options = {}) {
  const maxDepth = limitedInteger(options.maxDepth, JSON_TREE_DEFAULTS.maxDepth, 64);
  const maxNodes = limitedInteger(options.maxNodes, JSON_TREE_DEFAULTS.maxNodes, 5000);
  const maxStringLength = limitedInteger(options.maxStringLength, JSON_TREE_DEFAULTS.maxStringLength, 4096);
  const rows = [];
  const seen = new WeakMap();
  let nodeLimit = false;

  const visit = (current, path, parent, depth, key) => {
    if (rows.length >= maxNodes) { nodeLimit = true; return; }
    const type = kind(current);
    const entries = childrenOf(current);
    const expandable = type === 'array' || type === 'object';
    if (expandable && seen.has(current)) {
      rows.push({ path, parent, depth, key, type: 'circular', value: current, display: `[Circular → ${seen.get(current)}]`, expandable: false, childCount: 0, truncated: true });
      return;
    }
    if (expandable) seen.set(current, path);
    const bounded = expandable
      ? { display: `${type === 'array' ? 'Array' : 'Object'}(${entries.length})`, truncated: depth >= maxDepth && entries.length > 0 }
      : scalar(current, type, maxStringLength);
    rows.push({ path, parent, depth, key, type, value: current, display: bounded.display, expandable: expandable && entries.length > 0 && depth < maxDepth, childCount: entries.length, truncated: bounded.truncated });
    if (!expandable || entries.length === 0 || depth >= maxDepth) return;
    for (const [childKey, child] of entries) {
      visit(child, segment(path, childKey, type === 'array'), path, depth + 1, childKey);
      if (nodeLimit) break;
    }
  };
  visit(value, '$', null, 0, '$');
  if (nodeLimit) {
    if (rows.length === maxNodes) rows.pop();
    rows.push({ path: '$.[[truncated]]', parent: '$', depth: 1, key: '…', type: 'truncated', value: undefined, display: `Node limit reached; showing at most ${maxNodes} rows.`, expandable: false, childCount: 0, truncated: true });
  }
  return { rows, limits: { maxDepth, maxNodes, maxStringLength }, truncated: nodeLimit || rows.some((row) => row.truncated) };
}

/** Applies expansion or search while retaining every ancestor of a match. */
export function visibleJsonRows(rows, expanded = new Set(['$']), query = '') {
  const term = String(query).trim().toLowerCase();
  if (term) {
    const byPath = new Map(rows.map((row) => [row.path, row]));
    const retained = new Set();
    for (const row of rows) {
      if (`${row.path} ${row.type} ${row.display}`.toLowerCase().includes(term)) {
        let current = row;
        while (current) { retained.add(current.path); current = current.parent ? byPath.get(current.parent) : null; }
      }
    }
    return rows.filter((row) => retained.has(row.path));
  }
  const available = new Set(['$']);
  return rows.filter((row) => {
    const visible = row.path === '$' || available.has(row.parent);
    if (visible && row.expandable && expanded.has(row.path)) available.add(row.path);
    return visible;
  });
}

function tone(type) {
  if (type === 'string') return 'positive';
  if (type === 'number' || type === 'bigint') return 'accent';
  if (type === 'boolean') return 'warning';
  if (type === 'circular' || type === 'truncated') return 'danger';
  return 'neutral';
}

function copyText(row) {
  return row.type === 'string' ? row.value.slice(0, 4096) : row.display;
}

/** A bounded JSON/object inspector composed entirely from native components. */
export function JsonTree({ value, maxDepth, maxNodes, maxStringLength, initiallyExpanded = ['$'], onSelect, onCopy, height = 'fill', grow = true }) {
  const model = useMemo(() => inspectJson(value, { maxDepth, maxNodes, maxStringLength }), [value, maxDepth, maxNodes, maxStringLength]);
  const [expanded, setExpanded] = useState(() => new Set(initiallyExpanded));
  const [query, setQuery] = useState('');
  const rows = visibleJsonRows(model.rows, expanded, query);
  const toggle = (path) => setExpanded((current) => {
    const next = new Set(current);
    if (next.has(path)) next.delete(path); else next.add(path);
    return next;
  });
  return h(Column, { gap: 1, grow },
    h(Search, { value: query, placeholder: 'Filter paths, types, and values', onChange: (event) => setQuery(String(event.value ?? '')) }),
    model.truncated ? h(InlineMessage, { label: `Inspection is bounded to ${model.limits.maxNodes} nodes, depth ${model.limits.maxDepth}, and ${model.limits.maxStringLength} characters per string. Truncated values are marked.`, tone: 'warning' }) : null,
    rows.length === 0 ? h(EmptyState, { label: 'No matching values', detail: 'Clear or broaden the filter.' }) : null,
    h(Scroll, { height, grow: true }, h(Column, { gap: 1 }, ...rows.map((row) => h(Row, { key: row.path, gap: 1, pad: { start: { step: Math.min(row.depth, 16) } }, align: 'center', wrap: true },
      row.expandable ? h(Button, { label: `${expanded.has(row.path) ? 'Collapse' : 'Expand'} ${row.path}`, variant: 'ghost', onInvoke: () => toggle(row.path) }) : h(Text, { label: row.path, color: 'text-dim' }),
      h(Badge, { label: row.type, tone: tone(row.type) }),
      h(Button, { label: row.display, variant: 'ghost', onInvoke: () => onSelect?.({ path: row.path, type: row.type, value: row.value }) }),
      h(Button, { label: `Copy ${row.path}`, enabled: typeof onCopy === 'function', onInvoke: () => onCopy?.({ path: row.path, type: row.type, value: row.value, text: copyText(row) }) }),
      row.truncated ? h(Text, { label: `Truncated at ${row.path}`, color: 'warning' }) : null,
    )))),
  );
}

/** Discoverable alias for product surfaces that call this pattern an inspector. */
export const ObjectInspector = JsonTree;

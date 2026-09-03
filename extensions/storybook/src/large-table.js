import React from 'react';
import { Banner, Button, Column, DataTable, EmptyState, Entry, Heading, InlineMessage, Progress, Row, Select, Text } from '@husklet/react';

const { createElement: h, useRef, useState } = React;
export const LOGICAL_ROWS = 100_000;
export const WINDOW_LIMIT = 128;
export const OPERATION_HISTORY_LIMIT = 6;
export const SOURCE = 100;
export const SCHEMA = Object.freeze([
  { key: 'id', title: 'ID', width: { chars: 12 }, sortable: true },
  { key: 'name', title: 'Workspace record', width: 'fill', sortable: true, editable: true },
  { key: 'state', title: 'State', width: { chars: 12 } },
]);

/** A producer for a logical collection; it materializes only requested rows. */
export class LargeRecordSource {
  constructor(send = async () => {}) {
    this.send = send;
    this.version = 1;
    this.filter = '';
    this.descending = false;
    this.state = 'ready';
    this.generated = 0;
    this.edits = new Map();
  }

  length() {
    if (this.state === 'empty') return 0;
    if (this.state === 'error') return 1;
    return this.filter ? 1_000 : LOGICAL_ROWS;
  }

  async publish() {
    await this.send('source_resize', { mutation: { Length: { source: SOURCE, version: this.version, rows: this.length() } } });
  }

  async configure({ filter = this.filter, descending = this.descending, state = this.state }) {
    this.filter = filter.slice(0, 80);
    this.descending = Boolean(descending);
    this.state = state;
    this.version += 1;
    await this.publish();
  }

  answer(request) {
    if (request.source !== SOURCE || request.version !== this.version || this.state === 'loading') return null;
    const count = Math.min(request.range.count, WINDOW_LIMIT, Math.max(0, this.length() - request.range.start));
    const rows = Array.from({ length: count }, (_, offset) => this.row(request.range.start + offset));
    this.generated += rows.length;
    return { source: SOURCE, version: this.version, request: request.id, range: request.range, rows };
  }

  row(index) {
    if (this.state === 'error') {
      return { key: 0, cells: [{ Text: 'unavailable' }, { Text: 'The source refused this window' }, { Badge: { label: 'error', tone: 'Danger' } }] };
    }
    const logical = this.descending ? this.length() - index - 1 : index;
    return { key: logical, cells: [{ Number: logical }, { Text: this.edits.get(String(logical)) ?? `${this.filter || 'record'}-${logical}` }, { Badge: { label: logical % 3 ? 'ready' : 'busy', tone: logical % 3 ? 'Positive' : 'Warning' } }] };
  }

  async edit(event) {
    const current = event.source === SOURCE && event.version === this.version;
    const value = String(event.value ?? '').trim();
    if (!current) return { accepted: false, reason: 'stale version' };
    if (event.column !== 'name' || !event.row?.id || value.length === 0 || new TextEncoder().encode(value).length > 256) {
      return { accepted: false, reason: 'invalid value' };
    }
    this.edits.set(String(event.row.id), value);
    this.version += 1;
    await this.publish();
    return { accepted: true };
  }
}

export function LargeDataTableStory({ source }) {
  const [filter, setFilter] = useState('');
  const [descending, setDescending] = useState(false);
  const [state, setState] = useState('ready');
  const [selected, setSelected] = useState('No record selected');
  const [interactions, setInteractions] = useState([]);
  const sequence = useRef(0);
  const record = (label) => {
    const item = `#${++sequence.current} ${label}`;
    setInteractions((current) => [...current, item].slice(-OPERATION_HISTORY_LIMIT));
  };
  const update = (next) => {
    const changed = { filter, descending, state, ...next };
    setFilter(changed.filter); setDescending(changed.descending); setState(changed.state);
    void source.configure(changed);
  };
  return h(Column, { gap: 2, grow: true },
    h(Heading, { label: '100,000 logical records', scale: 'title', wrap: true }),
    h(Text, { label: 'Only host-requested 128-row windows exist in memory. Resize or scroll to request another window.', wrap: true }),
    h(Row, { gap: 2, wrap: true },
      h(Entry, {
        value: filter,
        placeholder: 'Filter records',
        onFocus: () => record('focused filter'),
        onChange: (event) => {
          update({ filter: String(event.value ?? '') });
          record('filtered records');
        },
      }),
      h(Button, {
        label: descending ? 'Sort ascending' : 'Sort descending',
        onFocus: () => record('focused sort'),
        onInvoke: () => {
          update({ descending: !descending });
          record(descending ? 'sorted ascending' : 'sorted descending');
        },
      }),
      h(Select, {
        value: state,
        choices: ['ready', 'loading', 'empty', 'error'].map((value) => ({ value, label: value })),
        onFocus: () => record('focused state'),
        onChange: (event) => {
          const next = String(event.value);
          update({ state: next });
          record(`state ${next}`);
        },
      }),
    ),
    ...(state === 'loading' ? [h(Progress, { key: 'loading', label: 'Waiting for a row window' })]
      : state === 'empty' ? [h(EmptyState, { key: 'empty', label: 'No matching records', detail: 'Change the filter or state control.' })]
      : state === 'error' ? [
        h(Banner, { key: 'error', label: 'The source rejected this window', tone: 'danger' }),
        h(Button, { key: 'retry', label: 'Retry row source', onInvoke: () => {
          update({ state: 'loading' });
          record('retrying row source');
        } }),
      ] : []),
    h(DataTable, {
      source: SOURCE,
      schema: SCHEMA,
      grow: true,
      onFocus: () => record('focused records'),
      onSelect: (event) => {
        const rows = Array.isArray(event.collection?.rows) ? event.collection.rows.slice(0, 1) : [];
        const current = event.collection?.source === SOURCE && event.collection?.version === source.version;
        const label = !current || rows.length === 0 ? 'No current record selected' : `Selected immutable record ${String(rows[0].id)}`;
        setSelected(label);
        record(label.toLowerCase());
      },
      onEdit: async (event) => {
        const result = await source.edit(event);
        record(result.accepted ? `renamed immutable record ${event.row.id}` : `edit refused: ${result.reason}`);
      },
    }),
    h(Text, { label: selected, color: 'text-dim' }),
    h(Text, { label: `Recent operations (${interactions.length}/${OPERATION_HISTORY_LIMIT})`, color: 'text-dim' }),
    ...(interactions.length === 0
      ? [h(InlineMessage, { label: 'Focus a control or select a record to inspect its bounded event history.', tone: 'neutral' })]
      : interactions.map((item) => h(InlineMessage, { key: item, label: item, tone: 'positive' }))),
  );
}

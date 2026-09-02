import React from 'react';
import { Banner, Button, Column, DataTable, EmptyState, Entry, Heading, Progress, Row, Select, Text } from '@husklet/react';

const { createElement: h, useState } = React;
export const LOGICAL_ROWS = 100_000;
export const WINDOW_LIMIT = 128;
export const SOURCE = 100;
export const SCHEMA = Object.freeze([
  { key: 'id', title: 'ID', width: { chars: 12 }, sortable: true },
  { key: 'name', title: 'Workspace record', width: 'fill', sortable: true },
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
      return { id: 0, cells: [{ Text: 'unavailable' }, { Text: 'The source refused this window' }, { Badge: { label: 'error', tone: 'danger' } }] };
    }
    const logical = this.descending ? this.length() - index - 1 : index;
    return { id: logical, cells: [{ Number: logical }, { Text: `${this.filter || 'record'}-${logical}` }, { Badge: { label: logical % 3 ? 'ready' : 'busy', tone: logical % 3 ? 'positive' : 'warning' } }] };
  }
}

export function LargeDataTableStory({ source }) {
  const [filter, setFilter] = useState('');
  const [descending, setDescending] = useState(false);
  const [state, setState] = useState('ready');
  const update = (next) => {
    const changed = { filter, descending, state, ...next };
    setFilter(changed.filter); setDescending(changed.descending); setState(changed.state);
    void source.configure(changed);
  };
  return h(Column, { gap: 2, grow: true },
    h(Heading, { label: '100,000 logical records', scale: 'title' }),
    h(Text, { label: 'Only host-requested 128-row windows exist in memory. Resize or scroll to request another window.', wrap: true }),
    h(Row, { gap: 2 },
      h(Entry, { value: filter, placeholder: 'Filter records', onChange: (event) => update({ filter: String(event.value ?? '') }) }),
      h(Button, { label: descending ? 'Sort ascending' : 'Sort descending', onInvoke: () => update({ descending: !descending }) }),
      h(Select, { value: state, choices: ['ready', 'loading', 'empty', 'error'].map((value) => ({ value, label: value })), onChange: (event) => update({ state: String(event.value) }) }),
    ),
    ...(state === 'loading' ? [h(Progress, { key: 'loading', label: 'Waiting for a row window' })]
      : state === 'empty' ? [h(EmptyState, { key: 'empty', label: 'No matching records', detail: 'Change the filter or state control.' })]
      : state === 'error' ? [h(Banner, { key: 'error', label: 'The source rejected this window', tone: 'danger' })] : []),
    h(DataTable, { source: SOURCE, schema: SCHEMA, grow: true }),
  );
}

import React from 'react';
import { Button, Column, Heading, InlineMessage, KeyValueTable, Row, Text } from '@husklet/react';
import type { ColumnSpec, InterfaceSourceMutation } from '@husklet/react';

const { useState } = React;

export const KEY_VALUE_STORY = 'Bounded key/value inspector';
export const KEY_VALUE_SOURCE = 102;
export const KEY_VALUE_RECORDS = 256;
export const KEY_VALUE_WINDOW_LIMIT = 32;
export const KEY_VALUE_SCHEMA: readonly ColumnSpec[] = Object.freeze([
  { key: 'key', title: 'Property', width: { chars: 22 } },
  { key: 'value', title: 'Value', width: 'fill' },
]);

type SourceSender = (_call: string, argument: { mutation: InterfaceSourceMutation }) => Promise<void>;
type WindowRequest = { source: number; version: number; id: number; range: { start: number; count: number } };

function windowRequest(value: unknown): WindowRequest | null {
  if (value === null || typeof value !== 'object') return null;
  const request = value as Record<string, unknown>;
  if (request.range === null || typeof request.range !== 'object') return null;
  const range = request.range as Record<string, unknown>;
  return Number.isSafeInteger(request.source) && Number.isSafeInteger(request.version)
    && Number.isSafeInteger(request.id) && Number.isSafeInteger(range.start) && Number(range.start) >= 0
    && Number.isSafeInteger(range.count) && Number(range.count) >= 0
    ? request as WindowRequest
    : null;
}

/** A manifest-like property supply which materializes only host-requested rows. */
export class KeyValueSource {
  readonly send: SourceSender;
  readonly version: number;
  generated: number;

  constructor(send: SourceSender = async () => {}) {
    this.send = send;
    this.version = 1;
    this.generated = 0;
  }

  async publish() {
    await this.send('source_resize', {
      mutation: { Length: { source: KEY_VALUE_SOURCE, version: this.version, rows: KEY_VALUE_RECORDS } },
    });
  }

  answer(value: unknown) {
    const request = windowRequest(value);
    if (!request) return null;
    if (request.source !== KEY_VALUE_SOURCE || request.version !== this.version) return null;
    const count = Math.min(request.range.count, KEY_VALUE_WINDOW_LIMIT,
      Math.max(0, KEY_VALUE_RECORDS - request.range.start));
    const rows = Array.from({ length: count }, (_, offset) => {
      const index = request.range.start + offset;
      return { id: index + 1, cells: [{ Text: `manifest.field.${index}` }, { Code: `value-${index}` }] };
    });
    this.generated += rows.length;
    return { source: KEY_VALUE_SOURCE, version: this.version, request: request.id, range: request.range, rows };
  }
}

export function KeyValueInspectorStory({ source: _source }: { source: KeyValueSource }) {
  const [refreshes, setRefreshes] = useState(0);
  return (
    <Column gap={2} grow={true}>
      <Heading label={'Image manifest inspector'} scale={'title'} wrap={true} />
      <Text
        label={'256 logical properties; GTK requests at most 32 rows at a time.'}
        wrap={true} />
      <KeyValueTable source={KEY_VALUE_SOURCE} schema={KEY_VALUE_SCHEMA} grow={true} />
      <Row gap={2} wrap={true}>
        <Button
          label={'Refresh metadata'}
          onInvoke={() => setRefreshes((count) => count + 1)} />
      </Row>
      <InlineMessage
        label={refreshes ? `Metadata refreshed ${refreshes} time${refreshes === 1 ? '' : 's'}.` : 'Metadata is current.'}
        tone={refreshes ? 'positive' : 'neutral'} />
    </Column>
  );
}

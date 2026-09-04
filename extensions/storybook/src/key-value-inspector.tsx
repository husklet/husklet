// @ts-nocheck -- legacy story typing is migrated incrementally.
import React from 'react';
import { Button, Column, Heading, InlineMessage, KeyValueTable, Row, Text } from '@husklet/react';

const { useState } = React;

export const KEY_VALUE_STORY = 'Bounded key/value inspector';
export const KEY_VALUE_SOURCE = 102;
export const KEY_VALUE_RECORDS = 256;
export const KEY_VALUE_WINDOW_LIMIT = 32;
export const KEY_VALUE_SCHEMA = Object.freeze([
  { key: 'key', title: 'Property', width: { chars: 22 } },
  { key: 'value', title: 'Value', width: 'fill' },
]);

/** A manifest-like property supply which materializes only host-requested rows. */
export class KeyValueSource {
  constructor(send = async () => {}) {
    this.send = send;
    this.version = 1;
    this.generated = 0;
  }

  async publish() {
    await this.send('source_resize', {
      mutation: { Length: { source: KEY_VALUE_SOURCE, version: this.version, rows: KEY_VALUE_RECORDS } },
    });
  }

  answer(request) {
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

export function KeyValueInspectorStory({ source }) {
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

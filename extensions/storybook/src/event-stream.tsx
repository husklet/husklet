// @ts-nocheck -- legacy story typing is migrated incrementally.
import React from 'react';
import { Button, Column, EventStream, Heading, InlineMessage, Row, Text } from '@husklet/react';

const { useState } = React;

export const EVENT_STREAM_STORY = 'Virtual event timeline';
export const EVENT_SOURCE = 101;
export const EVENT_RETENTION_LIMIT = 10_000;
export const EVENT_WINDOW_LIMIT = 64;
export const EVENT_SCHEMA = Object.freeze([
  { key: 'time', title: 'Time', width: { chars: 10 } },
  { key: 'kind', title: 'Event', width: { chars: 14 } },
  { key: 'detail', title: 'Detail', width: 'fill' },
]);

/** Fixed-retention history that materializes only the window requested by the host. */
export class TimelineSource {
  constructor(send = async () => {}) {
    this.send = send;
    this.version = 1;
    this.generated = 0;
    this.acknowledged = 0;
  }

  async publish() {
    await this.send('source_resize', {
      mutation: { Length: { source: EVENT_SOURCE, version: this.version, rows: EVENT_RETENTION_LIMIT } },
    });
  }

  answer(request) {
    if (request.source !== EVENT_SOURCE || request.version !== this.version) return null;
    const count = Math.min(request.range.count, EVENT_WINDOW_LIMIT,
      Math.max(0, EVENT_RETENTION_LIMIT - request.range.start));
    const rows = Array.from({ length: count }, (_, offset) => this.row(request.range.start + offset));
    this.generated += rows.length;
    return { source: EVENT_SOURCE, version: this.version, request: request.id, range: request.range, rows };
  }

  row(index) {
    const sequence = EVENT_RETENTION_LIMIT - index;
    return {
      id: sequence,
      cells: [
        { Text: `12:${String(sequence % 60).padStart(2, '0')}:00` },
        { Badge: { label: sequence % 5 ? 'completed' : 'warning', tone: sequence % 5 ? 'positive' : 'warning' } },
        { Text: `Deployment event ${sequence}` },
      ],
    };
  }
}

export function EventStreamStory({ source }) {
  const [acknowledged, setAcknowledged] = useState(0);
  return (
    <Column gap={2} grow={true}>
      <Heading label={'Operational event timeline'} scale={'title'} wrap={true} />
      <Text
        label={'The source retains 10,000 logical events; GTK requests and renders only a 64-row window.'}
        wrap={true} />
      <EventStream source={EVENT_SOURCE} schema={EVENT_SCHEMA} grow={true} />
      <Row gap={2} wrap={true}>
        <Button
          label={'Acknowledge newest'}
          onInvoke={() => {
            source.acknowledged += 1;
            setAcknowledged(source.acknowledged);
          }} />
      </Row>
      <InlineMessage
        label={acknowledged ? `Acknowledged newest event ${acknowledged} time${acknowledged === 1 ? '' : 's'}.` : 'No event acknowledged.'}
        tone={acknowledged ? 'positive' : 'neutral'} />
    </Column>
  );
}

// @ts-nocheck -- legacy story typing is migrated incrementally.
import React from 'react';
import { Column, Heading, InlineMessage, Text, TimelineView } from '@husklet/react';
export const TIMELINE_VIEW_STORY = 'Inspect incident timeline';
export const TIMELINE_LIMIT = 256;
export function boundedEvents(events) {
  const clean = (value) => String(value).replace(/[\t\r\n]/g, ' ');
  return events.filter(({ timestampMs, label }) => Number.isSafeInteger(timestampMs) && typeof label === 'string' && label.trim()).slice(0, TIMELINE_LIMIT).map(({ timestampMs, category = '', label, detail = '' }) => `${timestampMs}\t${clean(category)}\t${clean(label)}\t${clean(detail)}`).join('\n');
}
export function TimelineInspectionStory() {
  const value = boundedEvents([
    { timestampMs: 1700000000123, category: 'deploy', label: 'Release started', detail: 'image api:v2' },
    { timestampMs: 1700000000780, category: 'runtime', label: 'Replica replaced', detail: 'api-3' },
    { timestampMs: 1700000001456, category: 'health', label: 'Service ready', detail: '3/3 replicas' },
  ]);
  return (
    <Column gap={2} grow={true}>
      <Heading label={'Incident chronology'} scale={'title'} />
      <Text
        label={'Exact timestamps and event context remain selectable and semantically available.'} />
      <TimelineView value={value} tone={'accent'} grow={true} />
      <InlineMessage label={`Showing 3 of at most ${TIMELINE_LIMIT} events`} />
    </Column>
  );
}

// @ts-nocheck -- legacy story typing is migrated incrementally.
import React from 'react';
import { Column, Heading, InlineMessage, MemoryMap, Text } from '@husklet/react';

export const MEMORY_STORY = 'Inspect process memory';
export const REGION_LIMIT = 128;

export function boundedRegions(regions) {
  return regions
    .filter(({ start, end, permissions }) => Number.isSafeInteger(start) && Number.isSafeInteger(end) && start >= 0 && start < end && /^[rwxps-]{1,4}$/.test(permissions))
    .slice(0, REGION_LIMIT)
    .map(({ start, end, permissions, mapping = '' }) => `${start.toString(16).padStart(16, '0')}-${end.toString(16).padStart(16, '0')}\t${permissions}\t${end - start}\t${String(mapping).replace(/[\t\r\n]/g, ' ')}`)
    .join('\n');
}

export function MemoryInspectionStory() {
  const value = boundedRegions([
    { start: 0x400000, end: 0x410000, permissions: 'r-xp', mapping: '/workspace/bin/server' },
    { start: 0x610000, end: 0x614000, permissions: 'rw-p', mapping: '/workspace/bin/server' },
    { start: 0x800000, end: 0x828000, permissions: 'rw-p', mapping: '[heap]' },
    { start: 0x7fff0000, end: 0x80000000, permissions: 'rw-p', mapping: '[stack]' },
  ]);
  return (
    <Column gap={2} grow={true}>
      <Heading label={'Process address space'} scale={'title'} />
      <Text
        label={'Exact ranges, access permissions, byte sizes, and mappings remain selectable.'} />
      <MemoryMap value={value} tone={'accent'} grow={true} />
      <InlineMessage label={`Showing 4 of at most ${REGION_LIMIT} regions`} />
    </Column>
  );
}

import React from 'react';
import { Column, CoverageView, Heading, InlineMessage, Text } from '@husklet/react';
export const COVERAGE_STORY = 'Inspect source coverage'; export const COVERAGE_LIMIT = 512; export const SOURCE_LIMIT = 512;
type CoverageLine = { line: number; hits: number; source?: unknown };
export function boundedCoverage(lines: readonly unknown[], total: number = lines.length): string {
  const clean = (value: unknown): string => [...String(value).replace(/[\t\r\n]/g, ' ')].slice(0, SOURCE_LIMIT).join('');
  const kept = lines.filter((entry): entry is CoverageLine => {
    if (entry === null || typeof entry !== 'object') return false;
    const { line, hits } = entry as Record<string, unknown>;
    return Number.isSafeInteger(line) && Number(line) > 0 && Number.isSafeInteger(hits) && Number(hits) >= 0;
  }).slice(0, COVERAGE_LIMIT);
  const rows = kept.map(({ line, hits, source = '' }) => `${line}\t${hits}\t${clean(source)}`);
  if (Number.isSafeInteger(total) && total > kept.length) rows.push(`…\t\t… showing ${kept.length} of ${total} lines …`);
  return rows.join('\n');
}
export function CoverageInspectionStory() { const value = boundedCoverage([{ line: 41, hits: 8, source: 'match request {' }, { line: 42, hits: 8, source: '    Request::Ready => serve(),' }, { line: 43, hits: 0, source: '    Request::Retry => retry(),' }, { line: 44, hits: 8, source: '}' }], 287); return (
  <Column gap={2} grow={true}>
    <Heading label={'Source coverage'} scale={'title'} />
    <Text
      label={'Covered and missed lines remain selectable with exact hit counts.'} />
    <CoverageView value={value} tone={'warning'} grow={true} />
    <InlineMessage label={'Showing 4 of 287 lines'} />
  </Column>
); }

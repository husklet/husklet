import React from 'react';
import { Column, Heading, InlineMessage, TestReportView, Text } from '@husklet/react';
export const TEST_REPORT_STORY = 'Inspect test report'; export const CASE_LIMIT = 256; export const FAILURE_LIMIT = 512;
type TestCase = { suite: string; name: string; status: 'passed' | 'failed' | 'skipped'; durationMs: number; failure?: unknown };
export function boundedCases(cases: readonly unknown[]): string {
  const clean = (value: unknown): string => String(value).replace(/[\t\r\n]/g, ' ');
  return cases.filter((entry): entry is TestCase => {
    if (entry === null || typeof entry !== 'object') return false;
    const { suite, name, status, durationMs } = entry as Record<string, unknown>;
    return typeof suite === 'string' && Boolean(suite.trim()) && typeof name === 'string' && Boolean(name.trim())
      && (status === 'passed' || status === 'failed' || status === 'skipped')
      && Number.isSafeInteger(durationMs) && Number(durationMs) >= 0;
  }).slice(0, CASE_LIMIT).map(({ suite, name, status, durationMs, failure = '' }) => `${clean(suite)}\t${clean(name)}\t${status}\t${durationMs}\t${[...clean(failure)].slice(0, FAILURE_LIMIT).join('')}`).join('\n');
}
export function TestReportStory() {
  const value = boundedCases([
    { suite: 'auth', name: 'accepts valid token', status: 'passed', durationMs: 14 },
    { suite: 'auth', name: 'rejects expired token', status: 'failed', durationMs: 8, failure: 'expected 401, received 200' },
    { suite: 'storage', name: 'recovers journal', status: 'skipped', durationMs: 0, failure: 'requires integration fixture' },
  ]);
  return (
    <Column gap={2} grow={true}>
      <Heading label={'CI test report'} scale={'title'} />
      <Text
        label={'Suite, case, status, duration, and bounded failure detail remain selectable.'} />
      <TestReportView value={value} tone={'warning'} grow={true} />
      <InlineMessage label={`Showing 3 of at most ${CASE_LIMIT} cases`} />
    </Column>
  );
}

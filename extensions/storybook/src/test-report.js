import React from 'react';
import { Column, Heading, InlineMessage, TestReportView, Text } from '@husklet/react';
const { createElement: h } = React;
export const TEST_REPORT_STORY = 'Inspect test report'; export const CASE_LIMIT = 256; export const FAILURE_LIMIT = 512;
export function boundedCases(cases) {
  const clean = (value) => String(value).replace(/[\t\r\n]/g, ' ');
  return cases.filter(({ suite, name, status, durationMs }) => typeof suite === 'string' && suite.trim() && typeof name === 'string' && name.trim() && ['passed', 'failed', 'skipped'].includes(status) && Number.isSafeInteger(durationMs) && durationMs >= 0).slice(0, CASE_LIMIT).map(({ suite, name, status, durationMs, failure = '' }) => `${clean(suite)}\t${clean(name)}\t${status}\t${durationMs}\t${[...clean(failure)].slice(0, FAILURE_LIMIT).join('')}`).join('\n');
}
export function TestReportStory() {
  const value = boundedCases([
    { suite: 'auth', name: 'accepts valid token', status: 'passed', durationMs: 14 },
    { suite: 'auth', name: 'rejects expired token', status: 'failed', durationMs: 8, failure: 'expected 401, received 200' },
    { suite: 'storage', name: 'recovers journal', status: 'skipped', durationMs: 0, failure: 'requires integration fixture' },
  ]);
  return h(Column, { gap: 2, grow: true }, h(Heading, { label: 'CI test report', scale: 'title' }), h(Text, { label: 'Suite, case, status, duration, and bounded failure detail remain selectable.' }), h(TestReportView, { value, tone: 'warning', grow: true }), h(InlineMessage, { label: `Showing 3 of at most ${CASE_LIMIT} cases` }));
}

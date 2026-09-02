import React from 'react';
import { Card, CardContent, Column, Heading, Row, Sparkline, Stat, Text } from '@husklet/react';

const { createElement: h } = React;

export const METRICS_STORY = 'Inspect resource trends';
export const SAMPLE_LIMIT = 64;

export function boundedSamples(samples) {
  return samples.filter(Number.isFinite).slice(-SAMPLE_LIMIT).join(',');
}

export function ResourceMetricsStory() {
  const cpu = boundedSamples([18, 22, 19, 31, 28, 35, 42, 39]);
  const memory = boundedSamples([48, 49, 51, 52, 54, 57, 58, 61]);
  return h(Column, { gap: 2, grow: true },
    h(Heading, { label: 'Workspace resources', scale: 'title' }),
    h(Text, { label: 'Compact trends preserve their bounded samples in the semantic tree.' }),
    h(Row, { gap: 2, wrap: true },
      h(Card, {}, h(CardContent, { gap: 1 }, h(Stat, { label: 'CPU', value: '39%' }), h(Sparkline, { value: cpu, tone: 'accent' }))),
      h(Card, {}, h(CardContent, { gap: 1 }, h(Stat, { label: 'Memory', value: '61%' }), h(Sparkline, { value: memory, tone: 'positive' }))),
    ),
  );
}

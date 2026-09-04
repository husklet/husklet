import React from 'react';
import { Card, CardContent, Column, Heading, Row, Sparkline, Stat, Text } from '@husklet/react';


export const METRICS_STORY = 'Inspect resource trends';
export const SAMPLE_LIMIT = 64;

export function boundedSamples(samples: readonly unknown[]): string {
  return samples.filter((sample): sample is number => typeof sample === 'number' && Number.isFinite(sample)).slice(-SAMPLE_LIMIT).join(',');
}

export function ResourceMetricsStory() {
  const cpu = boundedSamples([18, 22, 19, 31, 28, 35, 42, 39]);
  const memory = boundedSamples([48, 49, 51, 52, 54, 57, 58, 61]);
  return (
    <Column gap={2} grow={true}>
      <Heading label={'Workspace resources'} scale={'title'} />
      <Text
        label={'Compact trends preserve their bounded samples in the semantic tree.'} />
      <Row gap={2} wrap={true}>
        <Card>
          <CardContent gap={1}>
            <Stat label={'CPU'} value={'39%'} />
            <Sparkline value={cpu} tone={'accent'} />
          </CardContent>
        </Card>
        <Card>
          <CardContent gap={1}>
            <Stat label={'Memory'} value={'61%'} />
            <Sparkline value={memory} tone={'positive'} />
          </CardContent>
        </Card>
      </Row>
    </Column>
  );
}

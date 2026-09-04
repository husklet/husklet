import React from 'react';
import { Badge, Card, CardContent, Column, Heading, JsonView, Row, Text } from '@husklet/react';


export const JSON_STORY = 'Inspect API response';

const RESPONSE = JSON.stringify({
  status: 'ready',
  workspace: { name: 'development', architecture: 'arm64' },
  panes: [{ slot: 'pane-1', occupant: 'terminal' }, { slot: 'pane-2', occupant: 'extension' }],
  message: 'punctuation inside strings stays literal: { value, [safe] }',
});

/** A nested developer payload, readable and copyable without a web view. */
export function JsonResponseStory() {
  return (
    <Column gap={2} grow={true}>
      <Heading label={'API response inspector'} scale={'title'} />
      <Row gap={2}>
        <Badge label={'200 OK'} tone={'positive'} />
        <Text value={'application/json'} />
      </Row>
      <Card>
        <CardContent>
          <JsonView value={RESPONSE} grow={true} />
        </CardContent>
      </Card>
    </Column>
  );
}

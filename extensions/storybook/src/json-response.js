import React from 'react';
import { Badge, Card, CardContent, Column, Heading, JsonView, Row, Text } from '@husklet/react';

const { createElement: h } = React;

export const JSON_STORY = 'Inspect API response';

const RESPONSE = JSON.stringify({
  status: 'ready',
  workspace: { name: 'development', architecture: 'arm64' },
  panes: [{ slot: 'pane-1', occupant: 'terminal' }, { slot: 'pane-2', occupant: 'extension' }],
  message: 'punctuation inside strings stays literal: { value, [safe] }',
});

/** A nested developer payload, readable and copyable without a web view. */
export function JsonResponseStory() {
  return h(
    Column,
    { gap: 2, grow: true },
    h(Heading, { label: 'API response inspector', scale: 'title' }),
    h(Row, { gap: 2 }, h(Badge, { label: '200 OK', tone: 'positive' }), h(Text, { value: 'application/json' })),
    h(Card, {}, h(CardContent, {}, h(JsonView, { value: RESPONSE, grow: true }))),
  );
}

// @ts-nocheck -- legacy story typing is migrated incrementally.
import React from 'react';
import { Button, Column, DiffLine, DiffViewer, Heading, Row, Text } from '@husklet/react';

const { createElement: h, useState } = React;

export const DIFF_STORY = 'Review configuration diff';

export function DiffReviewStory() {
  const [sideBySide, setSideBySide] = useState(false);
  const lines = [
    [' ', 'services:', 'neutral'],
    ['-', '  api: image: app:v1', 'danger'],
    ['+', '  api: image: app:v2', 'positive'],
    ['+', '  api: replicas: 3', 'positive'],
  ];
  return h(Column, { gap: 2 },
    h(Heading, { label: 'Review configuration diff', scale: 'title' }),
    h(Text, { label: 'Every bounded line remains selectable and semantically inspectable.' }),
    h(Row, { justify: 'end' },
      h(Button, { label: sideBySide ? 'Show unified' : 'Show side by side', onInvoke: () => setSideBySide(!sideBySide) })),
    h(DiffViewer, { orientation: sideBySide ? 'horizontal' : 'vertical', gap: 1 },
      ...lines.map(([status, value, tone], index) => h(DiffLine, { key: index, label: status, value, tone }))),
  );
}

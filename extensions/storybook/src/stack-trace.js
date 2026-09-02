import React from 'react';
import { Card, CardContent, Column, Heading, StackFrame, StackTrace, Text } from '@husklet/react';
const { createElement: h } = React;
export const STACK_STORY = 'Inspect crash stack';
export function StackTraceStory() {
  return h(Column, { gap: 2 },
    h(Heading, { label: 'Extension host exited unexpectedly', scale: 'title' }),
    h(Text, { value: 'Every function and source location remains independently selectable.' }),
    h(Card, {}, h(CardContent, {}, h(StackTrace, { gap: 1 },
      h(StackFrame, { label: 'host::dispatch', value: 'src/host.rs:42:17', tone: 'danger' }),
      h(StackFrame, { label: 'session::receive', value: 'src/session.rs:118:9' }),
      h(StackFrame, { label: 'main', value: 'src/main.rs:12:5' }),
    ))));
}

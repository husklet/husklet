import React from 'react';
import { Card, CardContent, Column, Heading, StackFrame, StackTrace, Text } from '@husklet/react';
export const STACK_STORY = 'Inspect crash stack';
export function StackTraceStory() {
  return (
    <Column gap={2}>
      <Heading label={'Extension host exited unexpectedly'} scale={'title'} />
      <Text
        value={'Every function and source location remains independently selectable.'} />
      <Card>
        <CardContent>
          <StackTrace gap={1}>
            <StackFrame label={'host::dispatch'} value={'src/host.rs:42:17'} tone={'danger'} />
            <StackFrame label={'session::receive'} value={'src/session.rs:118:9'} />
            <StackFrame label={'main'} value={'src/main.rs:12:5'} />
          </StackTrace>
        </CardContent>
      </Card>
    </Column>
  );
}

import React from 'react';
import { Column, FormControlLabel, Heading, InlineMessage, Switch, TerminalTranscript, Text } from '@husklet/react';
import type { TerminalTranscriptLine } from '@husklet/react';

const { useState } = React;

export const TERMINAL_TRANSCRIPT_STORY = 'Terminal transcript inspection';

const initial: readonly TerminalTranscriptLine[] = [
  { id: 'prompt', number: 418, timestamp: '12:04:08.190', text: '$ npm test', stream: 'stdout' },
  { id: 'suite', number: 419, timestamp: '12:04:08.351', text: '▶ protocol framing', stream: 'system', tone: 'accent' },
  { id: 'pass', number: 420, timestamp: '12:04:08.412', text: '  26 passing', stream: 'stdout', tone: 'positive' },
  { id: 'warning', number: 421, timestamp: '12:04:08.414', text: 'warning: output retained from bounded tail', stream: 'stderr' },
  { id: 'cursor', number: 422, timestamp: '12:04:08.415', text: '$ ', stream: 'stdout' },
];

export function TerminalTranscriptStory() {
  const [timestamps, setTimestamps] = useState(true);
  const [selected, setSelected] = useState('warning');
  const [status, setStatus] = useState('Select a line or invoke a transcript action.');
  return (
    <Column gap={2} grow={true}>
      <Heading label={'Agent-readable terminal transcript'} scale={'title'} />
      <Text
        label={'A bounded native projection with exact cursor placement, stream tone, selection, and explicit actions.'}
        wrap={true} />
      <FormControlLabel label={'Show timestamps'}>
        <Switch
          checked={timestamps}
          onToggle={(event) => setTimestamps(Boolean(event.value))} />
      </FormControlLabel>
      <TerminalTranscript
        lines={initial}
        lineNumbers={true}
        timestamps={timestamps}
        selected={selected}
        cursor={{ line: 422, column: 2 }}
        truncated={true}
        droppedLines={413}
        onSelect={(line) => { setSelected(String(line.id ?? line.number ?? '')); setStatus(`Selected immutable line ${line.number}.`); }}
        actions={[
          { id: 'copy', label: 'Copy visible', onInvoke: () => setStatus('Copied the bounded visible projection.') },
          { id: 'clear', label: 'Clear transcript', tone: 'danger', destructive: true, onInvoke: () => setStatus('Clear requested; confirmation remains a host concern.') },
        ]} />
      <InlineMessage label={status} tone={'neutral'} />
    </Column>
  );
}

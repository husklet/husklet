// @ts-nocheck -- legacy story typing is migrated incrementally.
import React from 'react';
import { Column, Heading, InlineMessage, Switch, TerminalTranscript, Text } from '@husklet/react';

const { createElement: h, useState } = React;

export const TERMINAL_TRANSCRIPT_STORY = 'Terminal transcript inspection';

const initial = [
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
  return h(Column, { gap: 2, grow: true },
    h(Heading, { label: 'Agent-readable terminal transcript', scale: 'title' }),
    h(Text, { label: 'A bounded native projection with exact cursor placement, stream tone, selection, and explicit actions.', wrap: true }),
    h(Switch, { label: 'Show timestamps', checked: timestamps, onChange: (event) => setTimestamps(Boolean(event?.value)) }),
    h(TerminalTranscript, {
      lines: initial, lineNumbers: true, timestamps, selected,
      cursor: { line: 422, column: 2 }, truncated: true, droppedLines: 413,
      onSelect: (line) => { setSelected(line.id); setStatus(`Selected immutable line ${line.number}.`); },
      actions: [
        { id: 'copy', label: 'Copy visible', onInvoke: () => setStatus('Copied the bounded visible projection.') },
        { id: 'clear', label: 'Clear transcript', tone: 'danger', destructive: true, onInvoke: () => setStatus('Clear requested; confirmation remains a host concern.') },
      ],
    }),
    h(InlineMessage, { label: status, tone: 'neutral' }),
  );
}

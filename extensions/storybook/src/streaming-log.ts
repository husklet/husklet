// @ts-nocheck -- legacy story typing is migrated incrementally.
import React from 'react';
import { Button, Column, Heading, InlineMessage, LogView, Row, Text } from '@husklet/react';

const { createElement: h, useRef, useState } = React;

export const STREAMING_LOG_STORY = 'Bounded streaming log';
export const LOG_CHUNK_LIMIT = 512;

function batch(sequence) {
  return Array.from({ length: 16 }, (_, index) =>
    `${String(sequence * 16 + index).padStart(5, '0')} worker-${index % 4} completed operation\n`).join('').slice(0, LOG_CHUNK_LIMIT);
}

export function StreamingLogStory() {
  const sequence = useRef(0);
  const [chunk, setChunk] = useState(() => `${'old history '.repeat(500)}\n${batch(0)}`);
  const [status, setStatus] = useState('Initial history exceeds retention so the oldest text is evicted.');
  const append = () => {
    sequence.current += 1;
    const next = batch(sequence.current);
    setChunk(next);
    setStatus(`Appended batch ${sequence.current}; ${next.length}/${LOG_CHUNK_LIMIT} wire characters.`);
  };
  return h(Column, { gap: 2, grow: true },
    h(Heading, { label: 'Bounded streaming operations', scale: 'title', wrap: true }),
    h(Text, { label: 'Each update sends only new text. GTK retains the newest 4,096 characters and follows the tail.', wrap: true }),
    h(LogView, { value: chunk, monospace: true, grow: true }),
    h(Row, { gap: 2, wrap: true }, h(Button, { label: 'Append batch', onInvoke: append })),
    h(InlineMessage, { label: status, tone: 'neutral' }),
  );
}

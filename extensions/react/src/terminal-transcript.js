import React from 'react';

import { Button, Column, InlineMessage, List, ListItemButton, Row, Text } from './components.js';

export const TERMINAL_TRANSCRIPT_LINE_LIMIT = 256;
export const TERMINAL_TRANSCRIPT_LINE_BYTE_LIMIT = 2_048;
export const TERMINAL_TRANSCRIPT_BYTE_LIMIT = 65_536;
export const TERMINAL_TRANSCRIPT_ACTION_LIMIT = 8;

const encoder = new TextEncoder();

function boundedText(value, limit) {
  let result = '';
  for (const character of String(value ?? '')) {
    if (encoder.encode(result + character).byteLength > limit) break;
    result += character;
  }
  return result;
}

function cursorText(text, column) {
  if (!Number.isSafeInteger(column) || column < 0) return text;
  const characters = [...text];
  const at = Math.min(column, characters.length);
  characters.splice(at, 0, '▉');
  return characters.join('');
}

/**
 * A bounded, selectable textual projection of a terminal screen or transcript.
 *
 * Lines are retained from the tail. This component deliberately renders native
 * Husklet nodes rather than a browser terminal emulator, so the same content and
 * actions remain available to GTK, keyboard users, and semantic readers.
 */
export function TerminalTranscript({
  lines = [], cursor, lineNumbers = false, timestamps = false, selected,
  truncated = false, droppedLines = 0, actions = [], onSelect,
  emptyLabel = 'No terminal output', ...props
}) {
  if (!Array.isArray(lines)) throw new TypeError('TerminalTranscript lines must be an array');
  if (!Array.isArray(actions)) throw new TypeError('TerminalTranscript actions must be an array');

  let bytes = 0;
  let clipped = Boolean(truncated) || lines.length > TERMINAL_TRANSCRIPT_LINE_LIMIT;
  const retained = [];
  for (const [sourceIndex, source] of lines.slice(-TERMINAL_TRANSCRIPT_LINE_LIMIT).entries()) {
    const line = typeof source === 'string' ? { text: source } : source;
    if (line === null || typeof line !== 'object') throw new TypeError('TerminalTranscript lines must be strings or objects');
    const text = boundedText(line.text, TERMINAL_TRANSCRIPT_LINE_BYTE_LIMIT);
    const size = encoder.encode(text).byteLength;
    if (bytes + size > TERMINAL_TRANSCRIPT_BYTE_LIMIT) {
      clipped = true;
      continue;
    }
    clipped ||= size < encoder.encode(String(line.text ?? '')).byteLength;
    bytes += size;
    retained.push({ ...line, text, sourceIndex: lines.length - Math.min(lines.length, TERMINAL_TRANSCRIPT_LINE_LIMIT) + sourceIndex });
  }

  const shownActions = actions.slice(0, TERMINAL_TRANSCRIPT_ACTION_LIMIT);
  clipped ||= actions.length > shownActions.length;
  return React.createElement(Column, { gap: 1, grow: true, ...props },
    clipped && React.createElement(InlineMessage, {
      key: 'truncated', tone: 'warning',
      label: `${Math.max(0, droppedLines)} earlier lines omitted; showing a bounded tail.`,
    }),
    retained.length === 0
      ? React.createElement(Text, { key: 'empty', label: emptyLabel, tone: 'neutral' })
      : React.createElement(List, { key: 'lines', grow: true }, ...retained.map((line) => {
        const number = line.number ?? line.sourceIndex + 1;
        const prefix = `${lineNumbers ? `${String(number).padStart(4, ' ')} ` : ''}${timestamps && line.timestamp ? `${line.timestamp} ` : ''}`;
        const text = cursor?.line === number ? cursorText(line.text, cursor.column) : line.text;
        return React.createElement(ListItemButton, {
          key: line.id ?? `${number}:${line.sourceIndex}`,
          label: `${prefix}${text}`,
          tone: line.tone ?? (line.stream === 'stderr' ? 'danger' : 'neutral'),
          selected: selected === (line.id ?? number),
          onInvoke: onSelect ? () => onSelect(line, line.sourceIndex) : undefined,
          tooltip: line.stream ? `${line.stream} line ${number}` : `terminal line ${number}`,
        });
      })),
    shownActions.length > 0 && React.createElement(Row, { key: 'actions', gap: 2, wrap: true },
      ...shownActions.map((action, index) => React.createElement(Button, {
        key: action.id ?? action.label ?? index,
        label: boundedText(action.label, 128),
        tone: action.tone ?? 'neutral',
        variant: action.variant ?? 'outline',
        destructive: Boolean(action.destructive),
        enabled: action.enabled ?? true,
        onInvoke: action.onInvoke,
      }))),
  );
}

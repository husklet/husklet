import React from 'react';

import { Button, Column, Expander, InlineMessage, Row, Text } from './components.js';

export const RECOVERY_DIAGNOSTIC_BYTE_LIMIT = 1024;
const encoder = new TextEncoder();

function bounded(value: unknown): string {
  let output = '';
  const source = value instanceof Error ? value.message : String(value ?? '');
  for (const character of source.trim()) {
    if (encoder.encode(output + character).byteLength > RECOVERY_DIAGNOSTIC_BYTE_LIMIT) break;
    output += character;
  }
  return output;
}

export function recoverySummary(error: unknown, operation = 'This view'): string {
  const detail = String(error ?? '').toLowerCase();
  if (/expected frame|received frame|sequence|out.of.order/.test(detail)) {
    return `${operation} lost sync with the extension host. No change was assumed.`;
  }
  if (/closed|broken pipe|connection|socket|channel/.test(detail)) {
    return `${operation} lost its connection. No change was assumed.`;
  }
  if (/timed? ?out|deadline/.test(detail)) return `${operation} did not respond in time.`;
  return `${operation} could not be completed.`;
}

export interface RecoveryStateProps extends Record<string, unknown> {
  error?: unknown;
  operation?: string;
  summary?: string;
  tone?: 'warning' | 'danger';
  retryLabel?: string;
  onRetry?: () => void;
}

/** Compact product-facing recovery with diagnostics disclosed only on request. */
export function RecoveryState({
  error,
  operation = 'This view',
  summary,
  tone = 'danger',
  retryLabel = 'Retry',
  onRetry,
  ...props
}: RecoveryStateProps) {
  if (onRetry !== undefined && typeof onRetry !== 'function') {
    throw new TypeError('RecoveryState onRetry must be a function');
  }
  const diagnostic = bounded(error);
  return React.createElement(
    Column,
    { ...props, gap: 1 },
    React.createElement(InlineMessage, {
      label: bounded(summary ?? recoverySummary(diagnostic, operation)),
      tone,
    }),
    onRetry
      ? React.createElement(
          Row,
          { align: 'start' },
          React.createElement(Button, {
            label: retryLabel,
            size: 'small',
            variant: 'outline',
            tone: 'accent',
            onInvoke: onRetry,
          }),
        )
      : null,
    diagnostic
      ? React.createElement(
          Expander!,
          { label: 'Technical details' },
          React.createElement(Text, { label: diagnostic, wrap: true }),
        )
      : null,
  );
}

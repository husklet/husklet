import React from 'react';
import { EmptyState, Progress } from './components.js';
import { RecoveryState } from './recovery-state.js';

export const RESOURCE_STATE_TEXT_BYTE_LIMIT = 1024;
const encoder = new TextEncoder();
function bounded(value: unknown): string {
  let output = '';
  for (const character of String(value ?? '')) {
    if (encoder.encode(output + character).byteLength > RESOURCE_STATE_TEXT_BYTE_LIMIT) break;
    output += character;
  }
  return output;
}

/** A consistent loading, empty, failure, or ready boundary for host resources. */
interface ResourceStateProps extends Record<string, unknown> {
  state: 'loading' | 'empty' | 'error' | 'ready';
  loadingLabel?: string;
  emptyLabel?: string;
  emptyDetail?: string;
  error?: string;
  retryLabel?: string;
  operation?: string;
  onRetry?: () => void;
  children?: React.ReactNode;
}
export function ResourceState({
  state,
  loadingLabel = 'Loading…',
  emptyLabel = 'Nothing here',
  emptyDetail = '',
  error = 'The resource could not be loaded.',
  retryLabel = 'Retry',
  operation = 'This view',
  onRetry,
  children,
  ...props
}: ResourceStateProps) {
  if (!['loading', 'empty', 'error', 'ready'].includes(state)) {
    throw new TypeError('ResourceState state must be loading, empty, error, or ready');
  }
  if (onRetry !== undefined && typeof onRetry !== 'function') {
    throw new TypeError('ResourceState onRetry must be a function');
  }
  if (state === 'ready') return React.createElement(React.Fragment, null, children);
  if (state === 'loading')
    return React.createElement(Progress, { ...props, label: bounded(loadingLabel) });
  if (state === 'empty')
    return React.createElement(EmptyState, {
      ...props,
      label: bounded(emptyLabel),
      detail: bounded(emptyDetail),
    });
  return React.createElement(RecoveryState, {
    ...props,
    error: bounded(error),
    operation,
    retryLabel: bounded(retryLabel),
    onRetry,
  });
}

import React from 'react';
import { Button, Column, EmptyState, InlineMessage, Progress } from './components.js';

export const RESOURCE_STATE_TEXT_BYTE_LIMIT = 1024;
const encoder = new TextEncoder();
function bounded(value) {
  let output = '';
  for (const character of String(value ?? '')) {
    if (encoder.encode(output + character).byteLength > RESOURCE_STATE_TEXT_BYTE_LIMIT) break;
    output += character;
  }
  return output;
}

/** A consistent loading, empty, failure, or ready boundary for host resources. */
export function ResourceState({
  state, loadingLabel = 'Loading…', emptyLabel = 'Nothing here', emptyDetail = '',
  error = 'The resource could not be loaded.', retryLabel = 'Retry', onRetry, children, ...props
}) {
  if (!['loading', 'empty', 'error', 'ready'].includes(state)) {
    throw new TypeError('ResourceState state must be loading, empty, error, or ready');
  }
  if (onRetry !== undefined && typeof onRetry !== 'function') {
    throw new TypeError('ResourceState onRetry must be a function');
  }
  if (state === 'ready') return React.createElement(React.Fragment, null, children);
  if (state === 'loading') return React.createElement(Progress, { ...props, label: bounded(loadingLabel) });
  if (state === 'empty') return React.createElement(EmptyState, { ...props, label: bounded(emptyLabel), detail: bounded(emptyDetail) });
  return React.createElement(Column, { ...props, gap: 1 },
    React.createElement(InlineMessage, { label: bounded(error), tone: 'danger' }),
    onRetry ? React.createElement(Button, { label: bounded(retryLabel), onInvoke: onRetry }) : null);
}

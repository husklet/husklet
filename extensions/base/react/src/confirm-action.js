import React, { useRef, useState } from 'react';

import { Button, Column, InlineMessage, Row, Spinner, Text } from './components.js';

export const CONFIRM_ACTION_TEXT_BYTE_LIMIT = 1024;
const LABEL_BYTE_LIMIT = 256;
const encoder = new TextEncoder();

function bounded(value, limit) {
  let output = '';
  for (const character of String(value ?? '')) {
    if (encoder.encode(output + character).byteLength > limit) break;
    output += character;
  }
  return output;
}

function authority(value) {
  if (typeof value !== 'string' || value.trim() === '' || encoder.encode(value).byteLength > LABEL_BYTE_LIMIT) {
    throw new TypeError('ConfirmAction authorityKey must be a nonblank string of at most 256 UTF-8 bytes');
  }
  return value;
}

function failure(cause) {
  const message = cause instanceof Error ? cause.message : String(cause ?? 'The operation failed.');
  return bounded(message || 'The operation failed.', CONFIRM_ACTION_TEXT_BYTE_LIMIT);
}

/** A two-stage async destructive action whose confirmation belongs to one stable authority. */
export function ConfirmAction({
  authorityKey,
  label,
  confirmLabel,
  question,
  onConfirm,
  enabled = true,
  cancelLabel = 'Cancel',
  pendingLabel = 'Working…',
  onCancel,
  ...props
}) {
  const currentAuthority = authority(authorityKey);
  if (typeof onConfirm !== 'function') throw new TypeError('ConfirmAction onConfirm must be a function');
  const epoch = useRef(0);
  const observed = useRef(currentAuthority);
  const [state, setState] = useState({ authority: '', phase: 'idle', error: '' });
  if (observed.current !== currentAuthority) {
    observed.current = currentAuthority;
    epoch.current += 1;
  }
  const active = state.authority === currentAuthority && state.phase !== 'idle';
  const pending = active && state.phase === 'pending';

  const open = () => {
    epoch.current += 1;
    setState({ authority: currentAuthority, phase: 'confirming', error: '' });
  };
  const cancel = () => {
    if (pending) return;
    epoch.current += 1;
    setState({ authority: '', phase: 'idle', error: '' });
    onCancel?.(currentAuthority);
  };
  const confirm = async () => {
    if (!active || pending || observed.current !== state.authority) return;
    const token = ++epoch.current;
    setState({ authority: currentAuthority, phase: 'pending', error: '' });
    try {
      await onConfirm(currentAuthority);
      if (epoch.current === token && observed.current === currentAuthority) {
        setState({ authority: '', phase: 'idle', error: '' });
      }
    } catch (cause) {
      if (epoch.current === token && observed.current === currentAuthority) {
        setState({ authority: currentAuthority, phase: 'confirming', error: failure(cause) });
      }
    }
  };

  if (!active) {
    return React.createElement(Button, {
      ...props,
      label: bounded(label, LABEL_BYTE_LIMIT),
      enabled: Boolean(enabled),
      tone: 'danger',
      onInvoke: open,
    });
  }
  return React.createElement(Column, { ...props, gap: 1 },
    React.createElement(Text, { label: bounded(question, CONFIRM_ACTION_TEXT_BYTE_LIMIT), color: 'warning', wrap: true }),
    React.createElement(Row, { gap: 1, align: 'center' },
      pending ? React.createElement(Spinner, { busy: true }) : null,
      React.createElement(Button, {
        label: pending ? bounded(pendingLabel, LABEL_BYTE_LIMIT) : bounded(confirmLabel, LABEL_BYTE_LIMIT),
        enabled: !pending,
        tone: 'danger',
        destructive: true,
        onInvoke: confirm,
      }),
      React.createElement(Button, {
        label: bounded(cancelLabel, LABEL_BYTE_LIMIT), enabled: !pending, onInvoke: cancel,
      })),
    state.error ? React.createElement(InlineMessage, { label: state.error, tone: 'danger' }) : null);
}

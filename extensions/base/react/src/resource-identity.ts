import React from 'react';
import { Code, Column, Text } from './components.js';

export const RESOURCE_IDENTITY_BYTE_LIMIT = 4096;
const encoder = new TextEncoder();

export interface ResourceIdentityProps {
  /** Short noun phrase naming the identifier, such as "Immutable network ID". */
  label: string;
  /** Complete copyable identity; never pass an abbreviated display value. */
  value: string;
}

/** A compact label plus the complete selectable identity used for exact resource actions. */
export function ResourceIdentity({ label, value }: ResourceIdentityProps) {
  if (!label.trim()) throw new TypeError('ResourceIdentity label must not be empty');
  if (!value.trim()) throw new TypeError('ResourceIdentity value must not be empty');
  if (encoder.encode(label).byteLength > 256)
    throw new RangeError('ResourceIdentity label must be at most 256 bytes');
  if (encoder.encode(value).byteLength > RESOURCE_IDENTITY_BYTE_LIMIT)
    throw new RangeError(
      `ResourceIdentity value must be at most ${RESOURCE_IDENTITY_BYTE_LIMIT} bytes`,
    );
  return React.createElement(
    Column,
    { gap: 0, width: 'fill' },
    React.createElement(Text, { label, color: 'text-dim' }),
    React.createElement(Code, { value, wrap: true, width: 'fill', tooltip: value }),
  );
}

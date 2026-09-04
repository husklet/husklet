// @ts-nocheck -- legacy story typing is migrated incrementally.
import React from 'react';
import { Column, Heading, HexView, InlineMessage, Text } from '@husklet/react';

const { createElement: h } = React;

export const BINARY_STORY = 'Inspect bounded binary data';
export const HEX_VIEW_BYTE_LIMIT = 4096;

/** Formats an exact byte value into the same stable projection as hl-gui. */
export function formatHex(bytes, totalBytes = bytes.length) {
  const shown = bytes.subarray(0, HEX_VIEW_BYTE_LIMIT);
  const lines = [];
  for (let offset = 0; offset < shown.length; offset += 16) {
    const row = shown.subarray(offset, offset + 16);
    const octets = Array.from(row, (byte) => byte.toString(16).padStart(2, '0'));
    const left = `${octets.slice(0, 8).join(' ').padEnd(23)}  ${octets.slice(8).join(' ').padEnd(23)}`;
    const printable = Array.from(row, (byte) => byte >= 0x20 && byte <= 0x7e ? String.fromCharCode(byte) : '.').join('');
    lines.push(`${offset.toString(16).padStart(8, '0')}  ${left}  |${printable}|`);
  }
  if (totalBytes > shown.length) lines.push(`… truncated: showing ${shown.length} of ${totalBytes} bytes …`);
  return lines.join('\n');
}

const ELF = Uint8Array.from([0x7f, 0x45, 0x4c, 0x46, 2, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 3, 0, 0x3e, 0]);

export function BinaryInspectionStory() {
  return h(Column, { gap: 2, grow: true },
    h(Heading, { label: 'Binary inspection', scale: 'title' }),
    h(Text, { label: 'Offsets, octets, and printable bytes remain selectable without decoding the source as text.' }),
    h(HexView, { value: formatHex(ELF, 8192), monospace: true, grow: true }),
    h(InlineMessage, { label: 'The source is bounded before formatting; omitted bytes are visible.', tone: 'neutral' }),
  );
}

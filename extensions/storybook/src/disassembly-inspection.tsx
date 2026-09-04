// @ts-nocheck -- legacy story typing is migrated incrementally.
import React from 'react';
import { Column, DisassemblyView, Heading, InlineMessage, Text } from '@husklet/react';

export const DISASSEMBLY_STORY = 'Inspect machine code';
export const INSTRUCTION_LIMIT = 256;

export function boundedInstructions(instructions) {
  return instructions.filter(({ address, bytes, mnemonic }) => Number.isSafeInteger(address) && address >= 0 && Array.isArray(bytes) && bytes.length > 0 && bytes.length <= 16 && typeof mnemonic === 'string' && mnemonic.trim()).slice(0, INSTRUCTION_LIMIT).map(({ address, bytes, mnemonic, operands = '' }) => `${address.toString(16).padStart(16, '0')}\t${bytes.map((byte) => byte.toString(16).padStart(2, '0')).join(' ')}\t${mnemonic.replace(/[\t\r\n]/g, ' ')}\t${String(operands).replace(/[\t\r\n]/g, ' ')}`).join('\n');
}

export function DisassemblyInspectionStory() {
  const value = boundedInstructions([
    { address: 0x401000, bytes: [0x55], mnemonic: 'push', operands: 'rbp' },
    { address: 0x401001, bytes: [0x48, 0x89, 0xe5], mnemonic: 'mov', operands: 'rbp, rsp' },
    { address: 0x401004, bytes: [0xe8, 0x27, 0, 0, 0], mnemonic: 'call', operands: '0x401030 <serve>' },
    { address: 0x401009, bytes: [0xc3], mnemonic: 'ret', operands: '' },
  ]);
  return (
    <Column gap={2} grow={true}>
      <Heading label={'Decoded entry point'} scale={'title'} />
      <Text
        label={'Addresses, original bytes, mnemonics, and operands remain selectable.'} />
      <DisassemblyView value={value} tone={'accent'} grow={true} />
      <InlineMessage label={`Showing 4 of at most ${INSTRUCTION_LIMIT} instructions`} />
    </Column>
  );
}

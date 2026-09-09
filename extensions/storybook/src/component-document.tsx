import React from 'react';
import {
  Code,
  Column,
  FormControl,
  FormHelperText,
  FormLabel,
  Heading,
  Section,
  Table,
  TableBody,
  TableCell,
  TableHead,
  TableRow,
  Text,
} from '@husklet/react';
import type { ControlRow } from './editors.js';

export function ComponentDocument({
  name,
  summary,
  children,
}: {
  name: string;
  summary: string;
  children: React.ReactNode;
}) {
  return (
    <Column width="fill" pad={4} gap={4}>
      <Heading label={name} scale="display" />
      <Text label={summary} color="text-dim" wrap />
      {children}
    </Column>
  );
}

export function FieldSpecimen({
  label,
  helper,
  width,
  children,
}: {
  label: string;
  helper?: string;
  width?: React.ComponentProps<typeof FormControl>['width'];
  children: React.ReactNode;
}) {
  return (
    <FormControl gap={1} width={width ?? 'fill'} align={width ? 'start' : 'stretch'}>
      <FormLabel label={label} />
      {children}
      {helper ? <FormHelperText label={helper} /> : null}
    </FormControl>
  );
}

export function DocumentationSection({
  title,
  children,
}: {
  title: string;
  children: React.ReactNode;
}) {
  return (
    <Section gap={2} width="fill">
      <Heading label={title} scale="title" />
      {children}
    </Section>
  );
}

export function ApiReference({ example, rows }: { example: string; rows: ControlRow[] }) {
  return (
    <Column gap={3} width="fill">
      <Code value={example} wrap />
      <Table width="fill">
        <TableHead>
          <TableRow>
            <TableCell label="Property" />
            <TableCell label="Control" />
            <TableCell label="Description" />
          </TableRow>
        </TableHead>
        <TableBody>
          {rows.map((row) => (
            <TableRow key={row.name}>
              <TableCell label={row.name} />
              <TableCell label={row.editor} />
              <TableCell label={row.note} />
            </TableRow>
          ))}
        </TableBody>
      </Table>
    </Column>
  );
}

export function caption(value: string) {
  return `${value[0].toUpperCase()}${value.slice(1)}`;
}

export function choices(values: readonly string[]) {
  return values.map((value) => ({ value, label: caption(value) }));
}

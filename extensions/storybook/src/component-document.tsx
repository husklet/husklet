import React from 'react';
import {
  Code,
  Column,
  Expander,
  FormControl,
  FormHelperText,
  FormLabel,
  Grid,
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

const INHERITED = new Set([
  'destructive',
  'visible',
  'tooltip',
  'width',
  'height',
  'pad',
  'align',
  'justify',
  'grow',
  'span',
  'rowSpan',
]);

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

export function SpecimenGrid({ children }: { children: React.ReactNode }) {
  return (
    <Grid columns={2} gap={3} width="fill">
      {children}
    </Grid>
  );
}

export function ApiReference({ example, rows }: { example: string; rows: ControlRow[] }) {
  const own = rows.filter((row) => !INHERITED.has(row.name));
  const inherited = rows.filter((row) => INHERITED.has(row.name));
  return (
    <Column gap={3} width="fill">
      <Code value={example} wrap />
      <ApiTable rows={own} />
      {inherited.length > 0 ? (
        <Expander
          label={`Inherited layout and automation props · ${inherited.length}`}
          width="fill"
        >
          <ApiTable rows={inherited} />
        </Expander>
      ) : null}
    </Column>
  );
}

function ApiTable({ rows }: { rows: ControlRow[] }) {
  return (
    <Table width="fill">
      <TableHead>
        <TableRow>
          <TableCell label="Property" wrap ellipsize={false} />
          <TableCell label="Type" wrap ellipsize={false} />
          <TableCell label="Default" wrap ellipsize={false} />
          <TableCell label="Description" wrap ellipsize={false} />
        </TableRow>
      </TableHead>
      <TableBody>
        {rows.map((row) => (
          <TableRow key={row.name}>
            <TableCell label={row.name} wrap ellipsize={false} />
            <TableCell label={publicType(row)} wrap ellipsize={false} />
            <TableCell label={defaultValue(row)} wrap ellipsize={false} />
            <TableCell label={row.note} wrap ellipsize={false} />
          </TableRow>
        ))}
      </TableBody>
    </Table>
  );
}

function publicType(row: ControlRow): string {
  if (row.members?.length) return row.members.map(({ value }) => `'${value}'`).join(' | ');
  const names = row.values.map((value) => {
    if (value === 'Text') return 'string';
    if (value === 'Flag') return 'boolean';
    if (value === 'Number' || value === 'Integer') return 'number';
    return value;
  });
  return [...new Set(names)].join(' | ') || 'unknown';
}

function defaultValue(row: ControlRow): string {
  const match = /defaults? to ([^;]+?)(?: when absent|$)/i.exec(row.note);
  return match?.[1] ?? '—';
}

export function caption(value: string) {
  return `${value[0].toUpperCase()}${value.slice(1)}`;
}

export function choices(values: readonly string[]) {
  return values.map((value) => ({ value, label: caption(value) }));
}

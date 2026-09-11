import React from 'react';
import {
  Code,
  Column,
  Expander,
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

const INHERITED_LAYOUT = new Set([
  'width',
  'height',
  'pad',
  'align',
  'justify',
  'grow',
  'span',
  'rowSpan',
]);
const INHERITED_BEHAVIOR = new Set(['destructive', 'visible', 'tooltip']);
const API_PROPERTY_WIDTH = { chars: 10 } as const;
const API_TYPE_WIDTH = { chars: 12 } as const;
const API_DEFAULT_WIDTH = { chars: 7 } as const;

export function ComponentDocument({
  name,
  summary,
  contentWidth,
  children,
}: {
  name: string;
  summary: string;
  contentWidth?: React.ComponentProps<typeof Column>['width'];
  children: React.ReactNode;
}) {
  return (
    <Column width="fill" pad={4} gap={4}>
      <Heading label={name} scale="display" />
      <Text label={summary} color="text-dim" width={contentWidth ?? 'fill'} wrap />
      {contentWidth ? (
        <Column gap={4} width={contentWidth}>
          {children}
        </Column>
      ) : (
        children
      )}
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
    <Column gap={3} width="fill">
      {children}
    </Column>
  );
}

export function ApiReference({ example, rows }: { example?: string; rows: ControlRow[] }) {
  const inheritedLayout = rows.filter((row) => INHERITED_LAYOUT.has(row.name));
  const inheritedBehavior = rows.filter((row) => INHERITED_BEHAVIOR.has(row.name));
  const compatibility = rows.filter((row) => row.compatibility);
  const own = rows.filter(
    (row) =>
      !INHERITED_LAYOUT.has(row.name) && !INHERITED_BEHAVIOR.has(row.name) && !row.compatibility,
  );
  return (
    <Column gap={3} width="fill">
      {example ? <Code value={example} wrap /> : null}
      <ApiTable rows={own} />
      {compatibility.length > 0 ? (
        <Expander label={`Compatibility props · ${compatibility.length}`} width="fill">
          <ApiTable rows={compatibility} />
        </Expander>
      ) : null}
      {inheritedBehavior.length > 0 ? (
        <Expander
          label={`Inherited behavior and visibility props · ${inheritedBehavior.length}`}
          width="fill"
        >
          <ApiTable rows={inheritedBehavior} />
        </Expander>
      ) : null}
      {inheritedLayout.length > 0 ? (
        <Expander label={`Inherited layout props · ${inheritedLayout.length}`} width="fill">
          <ApiTable rows={inheritedLayout} />
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
          <TableCell label="Property" width={API_PROPERTY_WIDTH} />
          <TableCell label="Type" width={API_TYPE_WIDTH} />
          <TableCell label="Default" width={API_DEFAULT_WIDTH} />
          <TableCell label="Description" width="fill" wrap ellipsize={false} />
        </TableRow>
      </TableHead>
      <TableBody>
        {rows.map((row) => (
          <TableRow key={row.name}>
            <TableCell label={row.name} tooltip={row.name} width={API_PROPERTY_WIDTH} />
            <TableCell label={publicType(row)} tooltip={publicType(row)} width={API_TYPE_WIDTH} />
            <TableCell
              label={defaultValue(row)}
              tooltip={defaultValue(row)}
              width={API_DEFAULT_WIDTH}
            />
            <TableCell label={row.note} width="fill" wrap ellipsize={false} />
          </TableRow>
        ))}
      </TableBody>
    </Table>
  );
}

function publicType(row: ControlRow): string {
  if (row.type) return row.type;
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
  return row.default ?? '—';
}

export function caption(value: string) {
  return `${value[0].toUpperCase()}${value.slice(1)}`;
}

export function choices(values: readonly string[]) {
  return values.map((value) => ({ value, label: caption(value) }));
}

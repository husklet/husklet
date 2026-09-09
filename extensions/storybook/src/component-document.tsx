import React from 'react';
import { Column, Heading, Section, Text } from '@husklet/react';

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
    <Column width={{ maximum: { chars: 106 } }} pad={4} gap={4}>
      <Heading label={name} scale="display" />
      <Text label={summary} color="text-dim" wrap />
      {children}
    </Column>
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

export function caption(value: string) {
  return `${value[0].toUpperCase()}${value.slice(1)}`;
}

export function choices(values: readonly string[]) {
  return values.map((value) => ({ value, label: caption(value) }));
}

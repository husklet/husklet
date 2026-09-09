import React from 'react';
import { Code, Column, Heading, IconButton, Row, Section, Text } from '@husklet/react';

export function IconButtonWorkbench() {
  const [event, setEvent] = React.useState('No interaction yet.');
  return (
    <Column width={{ maximum: { chars: 106 } }} pad={4} gap={4}>
      <Heading label="Icon button" scale="display" />
      <Text
        label="Icon buttons expose a familiar action where space is constrained. Every icon still needs a concise accessible label and tooltip."
        color="text-dim"
        wrap
      />
      <Block title="Basic">
        <Row gap={2} wrap align="center">
          <IconButton
            icon="view-refresh-symbolic"
            label="Refresh"
            tooltip="Refresh"
            tone="accent"
            onInvoke={() => setEvent('Refresh invoked')}
          />
          <Text label={event} color="text-dim" />
        </Row>
      </Block>
      <Block title="Sizes">
        <Row gap={2} wrap align="center">
          {(['small', 'medium', 'large'] as const).map((size) => (
            <Column key={size} gap={1} align="center">
              <IconButton
                icon="document-open-symbolic"
                label={`Open document, ${size}`}
                tooltip={`Open · ${size}`}
                size={size}
                variant="outline"
              />
              <Text label={title(size)} color="text-dim" />
            </Column>
          ))}
        </Row>
        <Text label="Square 28px · 36px · 44px hit areas" color="text-dim" />
      </Block>
      <Block title="Variants">
        <Row gap={2} wrap>
          {(['filled', 'outline', 'ghost', 'plain'] as const).map((variant) => (
            <IconButton
              key={variant}
              icon="edit-copy-symbolic"
              label={`Copy, ${variant}`}
              tooltip={`Copy · ${variant}`}
              variant={variant}
              tone="accent"
            />
          ))}
        </Row>
      </Block>
      <Block title="Tones and states">
        <Row gap={2} wrap>
          <IconButton icon="emblem-ok-symbolic" label="Approve" tooltip="Approve" tone="positive" />
          <IconButton
            icon="dialog-warning-symbolic"
            label="Review warning"
            tooltip="Review warning"
            tone="warning"
          />
          <IconButton icon="user-trash-symbolic" label="Delete" tooltip="Delete" tone="danger" />
          <IconButton
            icon="view-refresh-symbolic"
            label="Refresh unavailable"
            tooltip="Refresh unavailable"
            enabled={false}
          />
        </Row>
      </Block>
      <Block title="Accessibility">
        <Text
          label="The label names the action for assistive technology; the tooltip makes the same meaning available to pointer users. Do not use an icon alone when its meaning is ambiguous."
          wrap
        />
      </Block>
      <Block title="API">
        <Code
          value={
            '<IconButton icon="view-refresh-symbolic" label="Refresh" tooltip="Refresh" size="small" onInvoke={refresh} />'
          }
          wrap
        />
      </Block>
    </Column>
  );
}

function Block({ title: label, children }: { title: string; children: React.ReactNode }) {
  return (
    <Section gap={2} width="fill">
      <Heading label={label} scale="title" />
      {children}
    </Section>
  );
}

function title(value: string) {
  return `${value[0].toUpperCase()}${value.slice(1)}`;
}

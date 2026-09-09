import React from 'react';
import {
  Column,
  Expander,
  Heading,
  IconButton,
  Row,
  Section,
  Select,
  Switch,
  Text,
} from '@husklet/react';
import {
  ApiReference,
  ComponentDocument,
  DocumentationSection,
  SpecimenGrid,
} from './component-document.js';
import { rows } from './editors.js';

type Size = 'small' | 'medium' | 'large';
type Variant = 'filled' | 'outline' | 'ghost' | 'plain';
type Tone = 'neutral' | 'accent' | 'positive' | 'warning' | 'danger';

export function IconButtonWorkbench() {
  const [event, setEvent] = React.useState('No interaction yet.');
  const [size, setSize] = React.useState<Size>('medium');
  const [variant, setVariant] = React.useState<Variant>('outline');
  const [tone, setTone] = React.useState<Tone>('accent');
  const [enabled, setEnabled] = React.useState(true);
  return (
    <ComponentDocument
      name="IconButton"
      summary="Icon buttons expose a familiar action where space is constrained. Every icon still needs a concise accessible label and tooltip."
    >
      <Block title="Basic">
        <Row gap={2} wrap>
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
      <DocumentationSection title="API">
        <ApiReference
          example={'<IconButton icon="view-refresh-symbolic" label="Refresh" onInvoke={refresh} />'}
          rows={rows('IconButton')}
        />
      </DocumentationSection>
      <SpecimenGrid>
        <Block title="Sizes">
          <Row gap={2} wrap>
            {(['small', 'medium', 'large'] as const).map((size) => (
              <Column key={size} gap={1} align="center">
                <IconButton
                  icon="document-open-symbolic"
                  label={`Open document, ${size}`}
                  tooltip={`Open · ${size}`}
                  size={size}
                  align="center"
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
              <Column key={variant} gap={1} align="center">
                <IconButton
                  icon="edit-copy-symbolic"
                  label={`Copy, ${variant}`}
                  tooltip={`Copy · ${variant}`}
                  variant={variant}
                  tone="accent"
                  align="center"
                />
                <Text label={title(variant)} color="text-dim" />
              </Column>
            ))}
          </Row>
        </Block>
        <Block title="Tones">
          <Row gap={2} wrap>
            {(['neutral', 'accent', 'positive', 'warning', 'danger'] as const).map((value) => (
              <Column key={value} gap={1} align="center">
                <IconButton
                  icon="edit-copy-symbolic"
                  label={`Copy, ${value}`}
                  tooltip={title(value)}
                  tone={value}
                  align="center"
                />
                <Text label={title(value)} color="text-dim" />
              </Column>
            ))}
          </Row>
        </Block>
        <Block title="States">
          <Row gap={4} wrap>
            <Column gap={1} align="center">
              <IconButton
                icon="view-refresh-symbolic"
                label="Refresh enabled"
                tooltip="Refresh enabled"
                tone="accent"
                align="center"
              />
              <Text label="Enabled" color="text-dim" />
            </Column>
            <Column gap={1} align="center">
              <IconButton
                icon="view-refresh-symbolic"
                label="Refresh unavailable"
                tooltip="Refresh unavailable"
                enabled={false}
                align="center"
              />
              <Text label="Disabled" color="text-dim" />
            </Column>
          </Row>
        </Block>
      </SpecimenGrid>
      <Block title="Accessibility">
        <Text
          label="The label names the action for assistive technology; the tooltip makes the same meaning available to pointer users. Do not use an icon alone when its meaning is ambiguous."
          wrap
        />
      </Block>
      <Expander label="Playground" expanded={false} width="fill">
        <Column gap={2} pad={2}>
          <Row gap={3} wrap align="end">
            <Text label="Variant" color="text-dim" />
            <Select
              value={variant}
              choices={choices(['filled', 'outline', 'ghost', 'plain'])}
              onChange={(report) => setVariant(report.value as Variant)}
            />
            <Text label="Size" color="text-dim" />
            <Select
              value={size}
              choices={choices(['small', 'medium', 'large'])}
              onChange={(report) => setSize(report.value as Size)}
            />
            <Text label="Tone" color="text-dim" />
            <Select
              value={tone}
              choices={choices(['neutral', 'accent', 'positive', 'warning', 'danger'])}
              onChange={(report) => setTone(report.value as Tone)}
            />
            <Text label="Enabled" color="text-dim" />
            <Switch checked={enabled} onToggle={(report) => setEnabled(Boolean(report.value))} />
          </Row>
          <IconButton
            icon="view-refresh-symbolic"
            label="Preview refresh"
            tooltip="Preview refresh"
            size={size}
            variant={variant}
            tone={tone}
            enabled={enabled}
            align="center"
          />
        </Column>
      </Expander>
    </ComponentDocument>
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

function choices(values: readonly string[]) {
  return values.map((value) => ({ value, label: title(value) }));
}

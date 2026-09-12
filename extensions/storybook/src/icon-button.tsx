import React from 'react';
import {
  Code,
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
  FieldSpecimen,
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
      summary="Icon buttons expose a familiar action where space is constrained. Every icon needs a concise accessible label; that label is also the fallback tooltip."
    >
      <Block title="Overview">
        <Code value={'<IconButton icon="view-refresh-symbolic" label="Refresh" />'} wrap />
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
      <Block title="Tooltip override">
        <Text
          label="Label supplies the accessible name and fallback tooltip. Override tooltip only when pointer users need additional context."
          color="text-dim"
          wrap
        />
        <IconButton
          icon="edit-clear-symbolic"
          label="Reset font size"
          tooltip="Use the host default for font size"
          variant="ghost"
          align="start"
        />
      </Block>
      <Block title="Toolbar action">
        <Row gap={2} align="center">
          <Heading label="Containers" scale="title" />
          <IconButton
            icon="view-refresh-symbolic"
            label="Refresh containers"
            tooltip="Refresh containers"
            size="small"
            variant="ghost"
          />
        </Row>
        <Text
          label="Use a compact ghost icon for a familiar secondary action beside a page heading."
          color="text-dim"
          wrap
        />
      </Block>
      <Row gap={3} wrap width="fill" align="start">
        <Block title="Sizes" width={{ chars: 42 }}>
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
        <Block title="Variants" width={{ chars: 42 }}>
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
        <Block title="States" width={{ chars: 42 }}>
          <Row gap={3} wrap>
            <Column gap={1} align="center">
              <IconButton
                icon="view-refresh-symbolic"
                label="Refresh"
                tooltip="Resting"
                variant="outline"
                tone="accent"
                align="center"
              />
              <Text label="Rest" color="text-dim" />
            </Column>
            <Column gap={1} align="center">
              <IconButton
                icon="view-refresh-symbolic"
                label="Refresh"
                tooltip="Keyboard focus"
                variant="outline"
                tone="accent"
                align="center"
                onFocus={() => setEvent('Keyboard focus visible')}
              />
              <Text label="Focus" color="text-dim" />
            </Column>
            <Column gap={1} align="center">
              <IconButton
                icon="view-refresh-symbolic"
                label="Refresh"
                tooltip="Pressed"
                variant="filled"
                tone="accent"
                align="center"
              />
              <Text label="Pressed" color="text-dim" />
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
          <Text
            label="Focus keeps a visible ring. Pressed uses a filled surface during activation."
            color="text-dim"
            wrap
          />
        </Block>
      </Row>
      <Block title="Accessibility">
        <Text label="Label is the accessible name and fallback tooltip. Never omit it." wrap />
      </Block>
      <DocumentationSection title="API">
        <ApiReference rows={rows('IconButton')} />
      </DocumentationSection>
      <Expander label="Playground" expanded={false} width="fill">
        <Column gap={2} pad={2}>
          <Row gap={3} wrap align="end">
            <FieldSpecimen label="Variant" width={{ chars: 14 }}>
              <Select
                value={variant}
                choices={choices(['filled', 'outline', 'ghost', 'plain'])}
                onChange={(report) => setVariant(report.value as Variant)}
              />
            </FieldSpecimen>
            <FieldSpecimen label="Size" width={{ chars: 12 }}>
              <Select
                value={size}
                choices={choices(['small', 'medium', 'large'])}
                onChange={(report) => setSize(report.value as Size)}
              />
            </FieldSpecimen>
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

function Block({
  title: label,
  width = 'fill',
  children,
}: {
  title: string;
  width?: React.ComponentProps<typeof Section>['width'];
  children: React.ReactNode;
}) {
  return (
    <Section gap={2} width={width}>
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

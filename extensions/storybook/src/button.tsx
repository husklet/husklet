import React from 'react';
import {
  Button,
  Code,
  Column,
  Entry,
  Expander,
  Heading,
  InlineMessage,
  Row,
  Section,
  Select,
  Switch,
  Text,
} from '@husklet/react';
import { rows } from './editors.js';

type Size = 'small' | 'medium' | 'large';
type Variant = 'filled' | 'outline' | 'ghost' | 'plain';
type Tone = 'neutral' | 'accent' | 'danger';

export function ButtonWorkbench() {
  const [label, setLabel] = React.useState('Run task');
  const [size, setSize] = React.useState<Size>('medium');
  const [variant, setVariant] = React.useState<Variant>('filled');
  const [tone, setTone] = React.useState<Tone>('accent');
  const [enabled, setEnabled] = React.useState(true);
  const [event, setEvent] = React.useState('No interaction yet.');
  return (
    <ScrollDocument>
      <Heading label="Button" scale="display" />
      <Text
        label="Buttons start an immediate action. Use semantic size, emphasis, and tone; keep labels short and specific."
        color="text-dim"
        wrap
      />
      <SectionBlock title="Overview">
        <Row gap={2} wrap>
          <Button
            label={label}
            size={size}
            variant={variant}
            tone={tone}
            enabled={enabled}
            onInvoke={() => setEvent(`${label} invoked`)}
            onFocus={() => setEvent(`${label} focused`)}
            onKey={() => setEvent('Key received')}
            onPointer={() => setEvent('Pointer received')}
          />
          <InlineMessage label={event} tone="neutral" />
        </Row>
      </SectionBlock>
      <SectionBlock title="Basic">
        <Row gap={2} wrap>
          <Button label="Save changes" variant="filled" tone="accent" />
          <Button label="Cancel" variant="plain" tone="neutral" />
        </Row>
        <Text
          label="Pair one clear primary action with a lower-emphasis alternative."
          color="text-dim"
          wrap
        />
      </SectionBlock>
      <SectionBlock title="Variants">
        <Row gap={2} wrap>
          {(['filled', 'outline', 'ghost', 'plain'] as const).map((value) => (
            <Button key={value} label={title(value)} variant={value} tone="accent" />
          ))}
        </Row>
      </SectionBlock>
      <SectionBlock title="Sizes">
        <Row gap={2}>
          <Text label="Size" width={{ chars: 8 }} color="text-dim" />
          {(['filled', 'outline', 'ghost', 'plain'] as const).map((emphasis) => (
            <Text key={emphasis} label={title(emphasis)} width={{ chars: 10 }} color="text-dim" />
          ))}
        </Row>
        {(['small', 'medium', 'large'] as const).map((controlSize) => (
          <Row key={controlSize} gap={2} wrap>
            <Text label={title(controlSize)} width={{ chars: 8 }} color="text-dim" />
            {(['filled', 'outline', 'ghost', 'plain'] as const).map((emphasis) => (
              <Button
                key={emphasis}
                label="Action"
                width={{ chars: 10 }}
                size={controlSize}
                variant={emphasis}
                tone="accent"
              />
            ))}
          </Row>
        ))}
        <Text label="28px · 36px · 44px control heights" color="text-dim" />
      </SectionBlock>
      <SectionBlock title="Icons">
        <Row gap={2} wrap>
          <Button label="Add item" icon="list-add-symbolic" variant="filled" tone="accent" />
          <Button label="Delete" icon="user-trash-symbolic" variant="outline" tone="danger" />
          <Button label="Refresh" icon="view-refresh-symbolic" variant="ghost" />
        </Row>
      </SectionBlock>
      <SectionBlock title="Tones">
        <Row gap={2} wrap>
          <Button label="Neutral" variant="filled" tone="neutral" />
          <Button label="Accent" variant="filled" tone="accent" />
          <Button label="Danger" variant="filled" tone="danger" />
          <Button label="Positive" variant="filled" tone="positive" />
          <Button label="Warning" variant="filled" tone="warning" />
        </Row>
      </SectionBlock>
      <SectionBlock title="States">
        <Row gap={2} wrap>
          <Button label="Normal" variant="filled" tone="accent" />
          <Button
            label="Focus me"
            variant="outline"
            tone="accent"
            onFocus={() => setEvent('Focus state visible')}
          />
          <Button label="Disabled" enabled={false} />
        </Row>
        <Text
          label="Hover, pressed, and keyboard focus are rendered by the native host—interact with the live controls above."
          color="text-dim"
          wrap
        />
      </SectionBlock>
      <SectionBlock title="Accessibility">
        <Text
          label="Use a unique action label. Icon buttons still need an accessible label. Disabled actions should explain their prerequisite nearby."
          wrap
        />
      </SectionBlock>
      <Expander label="Playground" expanded={false} width="fill">
        <Column gap={2} pad={2}>
          <Entry
            value={label}
            placeholder="Button label"
            onChange={(report) => setLabel(String(report.value ?? '').slice(0, 48))}
          />
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
              choices={choices(['neutral', 'accent', 'danger'])}
              onChange={(report) => setTone(report.value as Tone)}
            />
            <Text label="Enabled" color="text-dim" />
            <Switch checked={enabled} onToggle={(report) => setEnabled(Boolean(report.value))} />
          </Row>
        </Column>
      </Expander>
      <SectionBlock title="API">
        <Code
          value={`<Button label="${label}" size="${size}" variant="${variant}" tone="${tone}" onInvoke={runTask} />`}
          wrap
        />
        <Column gap={1}>
          {rows('Button').map((row) => (
            <Text key={row.name} label={`${row.name} · ${row.note}`} color="text-dim" wrap />
          ))}
        </Column>
      </SectionBlock>
    </ScrollDocument>
  );
}

function ScrollDocument({ children }: { children: React.ReactNode }) {
  return (
    <Column width="fill" pad={4} gap={4}>
      {children}
    </Column>
  );
}

function SectionBlock({ title: heading, children }: { title: string; children: React.ReactNode }) {
  return (
    <Section gap={2} width="fill">
      <Heading label={heading} scale="title" />
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

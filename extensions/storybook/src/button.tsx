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
  Spacer,
  Switch,
  Text,
} from '@husklet/react';
import {
  ApiReference,
  ComponentDocument,
  DocumentationSection,
  FieldSpecimen,
  SpecimenGrid,
} from './component-document.js';
import { rows } from './editors.js';

type Size = 'small' | 'medium' | 'large';
type Variant = 'filled' | 'outline' | 'ghost' | 'plain';
type Tone = 'neutral' | 'accent' | 'positive' | 'warning' | 'danger';

export function ButtonWorkbench() {
  const [label, setLabel] = React.useState('Run task');
  const [size, setSize] = React.useState<Size>('medium');
  const [variant, setVariant] = React.useState<Variant>('filled');
  const [tone, setTone] = React.useState<Tone>('accent');
  const [enabled, setEnabled] = React.useState(true);
  const [event, setEvent] = React.useState('No interaction yet.');
  return (
    <ComponentDocument
      name="Button"
      summary="Buttons start an immediate action. Use semantic size, emphasis, and tone; keep labels short and specific."
      contentWidth={{ maximum: { chars: 64 } }}
    >
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
        <Code
          width="fill"
          value={`<Button
  label="${label}"
  size="${size}"
  variant="${variant}"
  tone="${tone}"
  onInvoke={runTask}
/>`}
          wrap
        />
      </SectionBlock>
      <SpecimenGrid>
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
      </SpecimenGrid>
      <SectionBlock title="Sizes">
        <Text
          label="Small, medium, and large draw 28px, 36px, and 44px chrome. Every size remains inside a native target at least 44px high, so compact layout does not reduce pointer or keyboard access."
          color="text-dim"
          wrap
        />
        {(['small', 'medium', 'large'] as const).map((controlSize) => (
          <Column key={controlSize} gap={1} width="fill">
            <Text
              label={`${title(controlSize)} · ${height(controlSize)}px visible chrome · ≥44px target`}
              color="text-dim"
            />
            <Row gap={2} wrap width="fill">
              {(['filled', 'outline', 'ghost', 'plain'] as const).map((emphasis) => (
                <Button
                  key={emphasis}
                  label={title(emphasis)}
                  size={controlSize}
                  variant={emphasis}
                  tone="accent"
                />
              ))}
            </Row>
          </Column>
        ))}
      </SectionBlock>
      <SpecimenGrid>
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
      </SpecimenGrid>
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
          <Button label="Saving…" busy variant="filled" tone="accent" />
        </Row>
        <Text
          label={
            'Busy actions retain their label, show activity, and cannot run twice. Hover, pressed, and keyboard focus are rendered by the native host.'
          }
          color="text-dim"
          wrap
        />
      </SectionBlock>
      <SectionBlock title="Inline reset action">
        <Row gap={1} width="fill" align="center" justify="stretch">
          <Text label="Product access · 5/6" color="text-dim" />
          <Spacer />
          <Button label="Clear product access" size="small" variant="ghost" />
        </Row>
        <Text
          label="Place a compact reset beside the value it affects. Keep it secondary to the page’s commit action."
          color="text-dim"
          wrap
        />
      </SectionBlock>
      <SectionBlock title="Accessibility">
        <Text
          label={
            'Use a unique action label. Icon buttons still need an accessible label.\nDisabled actions should explain their prerequisite nearby.'
          }
          wrap
        />
      </SectionBlock>
      <DocumentationSection title="API">
        <ApiReference rows={rows('Button')} />
      </DocumentationSection>
      <Expander label="Playground" expanded={false} width="fill">
        <Column gap={2} pad={2}>
          <FieldSpecimen
            label="Label"
            helper="Use a short verb phrase that names the result. Maximum 48 characters."
            width={{ minimum: { chars: 18 }, maximum: { chars: 36 } }}
          >
            <Entry
              value={label}
              placeholder="Button label"
              onChange={(report) => setLabel(String(report.value ?? '').slice(0, 48))}
            />
          </FieldSpecimen>
          <Row gap={2} wrap align="end">
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
            <FieldSpecimen label="Tone" width={{ chars: 14 }}>
              <Select
                value={tone}
                choices={choices(['neutral', 'accent', 'positive', 'warning', 'danger'])}
                onChange={(report) => setTone(report.value as Tone)}
              />
            </FieldSpecimen>
            <FieldSpecimen label="Availability" width={{ chars: 14 }}>
              <Switch checked={enabled} onToggle={(report) => setEnabled(Boolean(report.value))} />
            </FieldSpecimen>
          </Row>
        </Column>
      </Expander>
    </ComponentDocument>
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
function height(size: Size) {
  return { small: 28, medium: 36, large: 44 }[size];
}
function choices(values: readonly string[]) {
  return values.map((value) => ({ value, label: title(value) }));
}

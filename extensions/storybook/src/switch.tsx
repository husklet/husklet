import React from 'react';
import { Code, Column, Expander, Row, Select, Switch, Text } from '@husklet/react';
import { ComponentDocument, DocumentationSection, choices } from './component-document.js';
import { rows } from './editors.js';

type Context = 'setting' | 'permission' | 'feature';

export function SwitchWorkbench() {
  const [checked, setChecked] = React.useState(true);
  const [enabled, setEnabled] = React.useState(true);
  const [context, setContext] = React.useState<Context>('setting');
  const [event, setEvent] = React.useState('Toggle the preview to inspect its state.');
  const labels = {
    setting: 'Restore panes on launch',
    permission: 'Allow container inspection',
    feature: 'Enable preview features',
  };
  return (
    <ComponentDocument
      name="Switch"
      summary="Switch changes one independent setting immediately. Put the meaning in adjacent text; the control itself communicates on or off."
    >
      <DocumentationSection title="Overview">
        <Row gap={2} align="center">
          <Switch
            checked={checked}
            enabled={enabled}
            tooltip={labels[context]}
            onToggle={(report) => {
              const next = Boolean(report.value);
              setChecked(next);
              setEvent(next ? 'Setting enabled' : 'Setting disabled');
            }}
          />
          <Column gap={1}>
            <Text label={labels[context]} />
            <Text label={event} color="text-dim" />
          </Column>
        </Row>
      </DocumentationSection>
      <DocumentationSection title="Variants">
        <Column gap={3}>
          <Row gap={2} align="center">
            <Switch checked tooltip="On" />
            <Text label="On" />
          </Row>
          <Row gap={2} align="center">
            <Switch checked={false} tooltip="Off" />
            <Text label="Off" />
          </Row>
          <Row gap={2} align="center">
            <Switch checked enabled={false} tooltip="Managed setting" />
            <Text label="On · managed" color="text-dim" />
          </Row>
        </Column>
      </DocumentationSection>
      <DocumentationSection title="States and sizing">
        <Text
          label="Switch uses one consistent native hit target; do not shrink it for dense layouts. Use row spacing and concise copy to control density instead."
          wrap
        />
      </DocumentationSection>
      <DocumentationSection title="Accessibility">
        <Text
          label="Use a positive label that describes the enabled state. Do not use a switch for a one-time action or require color alone to distinguish its state."
          wrap
        />
      </DocumentationSection>
      <Expander label="Playground" expanded={false} width="fill">
        <Column gap={2} pad={2}>
          <Select
            value={context}
            choices={choices(['setting', 'permission', 'feature'])}
            tooltip="Example context"
            onChange={(report) => setContext(report.value as Context)}
          />
          <Switch
            checked={enabled}
            tooltip="Preview enabled"
            onToggle={(report) => setEnabled(Boolean(report.value))}
          />
        </Column>
      </Expander>
      <DocumentationSection title="API">
        <Code value={`<Switch checked={restorePanes} onToggle={setRestorePanes} />`} wrap />
        {rows('Switch').map((row) => (
          <Text key={row.name} label={`${row.name} · ${row.note}`} color="text-dim" wrap />
        ))}
      </DocumentationSection>
    </ComponentDocument>
  );
}

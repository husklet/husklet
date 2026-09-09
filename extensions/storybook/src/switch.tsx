import React from 'react';
import { Code, Column, Expander, FormControlLabel, Select, Switch, Text } from '@husklet/react';
import {
  ApiReference,
  ComponentDocument,
  DocumentationSection,
  FieldSpecimen,
  SpecimenGrid,
  choices,
} from './component-document.js';
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
        <Code value={'<Switch checked={restorePanes} onToggle={setRestorePanes} />'} wrap />
        <Column gap={1}>
          <FormControlLabel label={labels[context]} gap={2}>
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
          </FormControlLabel>
          <Text label={event} color="text-dim" />
        </Column>
      </DocumentationSection>
      <DocumentationSection title="States">
        <SpecimenGrid>
          <FormControlLabel label="Restore panes · on" gap={2}>
            <Switch checked />
          </FormControlLabel>
          <FormControlLabel label="Restore panes · off" gap={2}>
            <Switch checked={false} />
          </FormControlLabel>
          <FieldSpecimen label="Focused" helper="Keyboard focus remains visible">
            <FormControlLabel label="Restore panes" gap={2}>
              <Switch checked tooltip="Focused restore switch" />
            </FormControlLabel>
          </FieldSpecimen>
          <FieldSpecimen label="Disabled · off" helper="Controlled by workspace policy">
            <FormControlLabel label="Restore panes" gap={2}>
              <Switch checked={false} enabled={false} />
            </FormControlLabel>
          </FieldSpecimen>
          <FieldSpecimen label="Disabled · on" helper="Controlled by workspace policy">
            <FormControlLabel label="Restore panes" gap={2}>
              <Switch checked enabled={false} />
            </FormControlLabel>
          </FieldSpecimen>
        </SpecimenGrid>
      </DocumentationSection>
      <DocumentationSection title="Sizing">
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
      <DocumentationSection title="Choose the right control">
        <Text
          label="Use Switch for an immediately applied setting, ToggleButton for a persistent toolbar option, and Checkbox when several choices are selected before a form is submitted."
          wrap
        />
      </DocumentationSection>
      <DocumentationSection title="API">
        <ApiReference rows={rows('Switch')} />
      </DocumentationSection>
      <Expander label="Playground" expanded={false} width="fill">
        <Column gap={2} pad={2}>
          <FieldSpecimen label="Example context" width={{ chars: 30 }}>
            <Select
              value={context}
              choices={choices(['setting', 'permission', 'feature'])}
              tooltip="Example context"
              width={{ chars: 30 }}
              onChange={(report) => setContext(report.value as Context)}
            />
          </FieldSpecimen>
          <FormControlLabel label="Preview enabled" gap={2}>
            <Switch
              checked={enabled}
              tooltip="Preview enabled"
              onToggle={(report) => setEnabled(Boolean(report.value))}
            />
          </FormControlLabel>
        </Column>
      </Expander>
    </ComponentDocument>
  );
}

import React from 'react';
import { Code, Column, Expander, Row, Select, Switch, Text } from '@husklet/react';
import { ComponentDocument, DocumentationSection } from './component-document.js';
import { rows } from './editors.js';

const shells = [
  { value: 'zsh', label: 'Z shell' },
  { value: 'bash', label: 'Bash' },
  { value: 'fish', label: 'Fish' },
];

export function SelectWorkbench() {
  const [value, setValue] = React.useState('zsh');
  const [enabled, setEnabled] = React.useState(true);
  const [wide, setWide] = React.useState(false);
  const [event, setEvent] = React.useState('Choose an option to inspect its value.');
  return (
    <ComponentDocument
      name="Select"
      summary="Select chooses one value from a short, stable set. Use labels people recognize and store the separate machine value."
    >
      <DocumentationSection title="Overview">
        <Select
          value={value}
          choices={shells}
          width={wide ? { chars: 42 } : { chars: 24 }}
          align="start"
          enabled={enabled}
          tooltip="Default shell"
          onChange={(report) => {
            const next = String(report.value);
            setValue(next);
            setEvent(`Selected · ${shells.find((choice) => choice.value === next)?.label ?? next}`);
          }}
          onFocus={() => setEvent('Selector focused')}
        />
        <Text label={event} color="text-dim" />
      </DocumentationSection>
      <DocumentationSection title="Widths">
        <Column gap={2}>
          <Select
            value="zsh"
            choices={shells}
            width={{ chars: 18 }}
            align="start"
            tooltip="Compact shell selector"
          />
          <Select
            value="bash"
            choices={shells}
            width={{ chars: 30 }}
            align="start"
            tooltip="Default shell selector"
          />
          <Select value="fish" choices={shells} width="fill" tooltip="Full-width shell selector" />
        </Column>
      </DocumentationSection>
      <DocumentationSection title="States">
        <Row gap={2} wrap>
          <Select value="zsh" choices={shells} tooltip="Enabled selector" />
          <Select
            value="bash"
            choices={shells}
            enabled={false}
            tooltip="Unavailable until restart"
          />
        </Row>
        <Text
          label="Keep the selected option visible; explain why an unavailable selector is disabled."
          color="text-dim"
          wrap
        />
      </DocumentationSection>
      <DocumentationSection title="Accessibility">
        <Text
          label="Pair the control with a visible field label. Option labels must be unique when spoken aloud, and keyboard focus must remain visible."
          wrap
        />
      </DocumentationSection>
      <Expander label="Playground" expanded={false} width="fill">
        <Row gap={3} pad={2} wrap>
          <Switch
            checked={enabled}
            tooltip="Enabled"
            onToggle={(report) => setEnabled(Boolean(report.value))}
          />
          <Switch
            checked={wide}
            tooltip="Wide field"
            onToggle={(report) => setWide(Boolean(report.value))}
          />
        </Row>
      </Expander>
      <DocumentationSection title="API">
        <Code value={`<Select value={shell} choices={shells} onChange={setShell} />`} wrap />
        {rows('Select').map((row) => (
          <Text key={row.name} label={`${row.name} · ${row.note}`} color="text-dim" wrap />
        ))}
      </DocumentationSection>
    </ComponentDocument>
  );
}

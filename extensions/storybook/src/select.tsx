import React from 'react';
import { Code, Column, Expander, FormControlLabel, Select, Switch, Text } from '@husklet/react';
import {
  ApiReference,
  ComponentDocument,
  DocumentationSection,
  FieldSpecimen,
  SpecimenGrid,
} from './component-document.js';
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
        <Code value={'<Select value={shell} choices={shells} onChange={setShell} />'} wrap />
        <FieldSpecimen
          label="Default shell"
          helper={event}
          width={wide ? { chars: 42 } : { chars: 24 }}
        >
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
              setEvent(
                `Selected · ${shells.find((choice) => choice.value === next)?.label ?? next}`,
              );
            }}
            onFocus={() => setEvent('Selector focused')}
          />
        </FieldSpecimen>
      </DocumentationSection>
      <DocumentationSection title="Widths">
        <Column gap={2}>
          <FieldSpecimen label="Compact · 18ch" width={{ chars: 18 }}>
            <Select value="zsh" choices={shells} width={{ chars: 18 }} />
          </FieldSpecimen>
          <FieldSpecimen label="Default · 30ch" width={{ chars: 30 }}>
            <Select value="zsh" choices={shells} width={{ chars: 30 }} />
          </FieldSpecimen>
          <FieldSpecimen label="Full width">
            <Select value="zsh" choices={shells} width="fill" />
          </FieldSpecimen>
        </Column>
      </DocumentationSection>
      <DocumentationSection title="States">
        <SpecimenGrid>
          <FieldSpecimen
            label="Empty"
            helper="Ask for a choice without inventing one"
            width={{ chars: 30 }}
          >
            <Select value="" choices={shells} width={{ chars: 30 }} />
          </FieldSpecimen>
          <FieldSpecimen
            label="Focused"
            helper="Keyboard focus remains visible"
            width={{ chars: 30 }}
          >
            <Select
              value="zsh"
              choices={shells}
              tooltip="Focused shell selector"
              width={{ chars: 30 }}
            />
          </FieldSpecimen>
          <FieldSpecimen label="Selected" helper="Changes apply to new panes" width={{ chars: 30 }}>
            <Select value="zsh" choices={shells} width={{ chars: 30 }} />
          </FieldSpecimen>
          <FieldSpecimen
            label="Disabled"
            helper="Restart the workspace to change this value"
            width={{ chars: 30 }}
          >
            <Select value="bash" choices={shells} enabled={false} width={{ chars: 30 }} />
          </FieldSpecimen>
          <FieldSpecimen label="Invalid" helper="Choose a default shell" width={{ chars: 30 }}>
            <Select value="" choices={shells} tone="danger" width={{ chars: 30 }} />
          </FieldSpecimen>
          <FieldSpecimen
            label="Long label"
            helper="The value remains bounded by the field"
            width={{ chars: 30 }}
          >
            <Select
              value="long"
              choices={[
                { value: 'long', label: 'Remote development shell with workspace defaults' },
              ]}
              width={{ chars: 30 }}
            />
          </FieldSpecimen>
        </SpecimenGrid>
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
      <DocumentationSection title="API">
        <ApiReference rows={rows('Select')} />
      </DocumentationSection>
      <Expander label="Playground" expanded={false} width="fill">
        <SpecimenGrid>
          <FormControlLabel label="Selector enabled" gap={2}>
            <Switch
              checked={enabled}
              tooltip="Selector enabled"
              onToggle={(report) => setEnabled(Boolean(report.value))}
            />
          </FormControlLabel>
          <FormControlLabel label="Use wide field" gap={2}>
            <Switch
              checked={wide}
              tooltip="Use wide field"
              onToggle={(report) => setWide(Boolean(report.value))}
            />
          </FormControlLabel>
        </SpecimenGrid>
      </Expander>
    </ComponentDocument>
  );
}

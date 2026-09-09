import React from 'react';
import {
  Code,
  Column,
  Expander,
  FormControlLabel,
  InlineMessage,
  Row,
  Switch,
  Text,
  ToggleButton,
} from '@husklet/react';
import {
  ApiReference,
  ComponentDocument,
  DocumentationSection,
  FieldSpecimen,
  SpecimenGrid,
} from './component-document.js';
import { rows } from './editors.js';

export function ToggleButtonWorkbench() {
  const [selected, setSelected] = React.useState(true);
  const [enabled, setEnabled] = React.useState(true);
  const [event, setEvent] = React.useState('No change yet.');

  function toggle(value: unknown) {
    const next = value === null ? !selected : Boolean(value);
    setSelected(next);
    setEvent(`Pin tab changed to ${next ? 'selected' : 'unselected'}.`);
  }

  return (
    <ComponentDocument
      name="Toggle Button"
      summary="Toggle buttons switch one persistent option on or off. Their selected state stays visible after activation, unlike an action button."
    >
      <DocumentationSection title="Overview">
        <Row gap={3} wrap align="center">
          <ToggleButton
            label="Pin tab"
            checked={selected}
            enabled={enabled}
            tooltip="Pin this tab"
            onToggle={(report) => toggle(report.value)}
          />
          <InlineMessage label={event} tone="neutral" />
        </Row>
        <Code
          value={`<ToggleButton label="Pin tab" checked={${selected}} onToggle={setPinned} />`}
          wrap
        />
      </DocumentationSection>

      <DocumentationSection title="Selected and unselected">
        <SpecimenGrid>
          <FieldSpecimen
            label="Selected"
            helper="The option is active. Keep the same label when its state changes."
          >
            <ToggleButton label="Pin tab" checked />
          </FieldSpecimen>
          <FieldSpecimen
            label="Unselected"
            helper="The option is available but currently inactive."
          >
            <ToggleButton label="Pin tab" checked={false} />
          </FieldSpecimen>
        </SpecimenGrid>
      </DocumentationSection>

      <SpecimenGrid>
        <DocumentationSection title="When to use">
          <Text
            label="Use it for an independent setting such as pinning, muting, or showing a panel. Use a Switch for a setting row and a ToggleButtonGroup when choices belong together."
            wrap
          />
        </DocumentationSection>
        <DocumentationSection title="Accessibility">
          <Text
            label="Write a stable label that names the option, not an instruction such as ‘Click to pin’. Selected state is exposed as pressed and must remain understandable without color. Space or Enter toggles the focused control."
            wrap
          />
        </DocumentationSection>
      </SpecimenGrid>

      <DocumentationSection title="API">
        <ApiReference rows={rows('ToggleButton')} />
      </DocumentationSection>

      <Expander label="Playground" expanded={false} width="fill">
        <Column gap={2} pad={2}>
          <FieldSpecimen
            label="Current value"
            helper="The live specimen above is controlled by this value."
          >
            <FormControlLabel label={selected ? 'Selected' : 'Unselected'} gap={2}>
              <Switch checked={selected} onToggle={(report) => toggle(report.value)} />
            </FormControlLabel>
          </FieldSpecimen>
          <FieldSpecimen label="Availability">
            <FormControlLabel label={enabled ? 'Enabled' : 'Disabled'} gap={2}>
              <Switch checked={enabled} onToggle={(report) => setEnabled(Boolean(report.value))} />
            </FormControlLabel>
          </FieldSpecimen>
        </Column>
      </Expander>
    </ComponentDocument>
  );
}

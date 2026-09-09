import React from 'react';
import { Checkbox, Code, Column, InlineMessage, Text } from '@husklet/react';
import {
  ApiReference,
  ComponentDocument,
  DocumentationSection,
  FieldSpecimen,
  SpecimenGrid,
} from './component-document.js';
import { rows } from './editors.js';

export function CheckboxWorkbench() {
  const [checked, setChecked] = React.useState(true);

  function toggle(value: unknown) {
    const next = value === null ? !checked : Boolean(value);
    setChecked(next);
  }

  return (
    <ComponentDocument
      name="Checkbox"
      summary="Checkbox lets people include or exclude one option in a form. Its label and indicator form one compact click target."
    >
      <DocumentationSection title="Overview">
        <Column gap={1}>
          <Checkbox
            label="Include diagnostics"
            checked={checked}
            tooltip="Include diagnostics"
            onToggle={(report) => toggle(report.value)}
          />
          <InlineMessage
            label={`Include diagnostics is ${checked ? 'checked' : 'unchecked'}.`}
            tone="neutral"
          />
        </Column>
        <Code
          value={
            '<Checkbox label="Include diagnostics" checked={included} onToggle={setIncluded} />'
          }
          wrap
        />
      </DocumentationSection>

      <DocumentationSection title="States">
        <SpecimenGrid>
          <FieldSpecimen label="Enabled · unchecked">
            <Checkbox label="Email updates" checked={false} />
          </FieldSpecimen>
          <FieldSpecimen label="Enabled · checked">
            <Checkbox label="Email updates" checked />
          </FieldSpecimen>
          <FieldSpecimen label="Disabled · unchecked" helper="Unavailable under current policy">
            <Checkbox label="Email updates" checked={false} enabled={false} />
          </FieldSpecimen>
          <FieldSpecimen label="Disabled · checked" helper="Required by current policy">
            <Checkbox label="Email updates" checked enabled={false} />
          </FieldSpecimen>
        </SpecimenGrid>
      </DocumentationSection>

      <DocumentationSection title="Keyboard and accessibility">
        <Text
          label="Tab moves focus to the labeled checkbox. Its native focus ring remains visible, and Space toggles the checked state. Keep the label clickable and describe the option, not the action."
          wrap
        />
      </DocumentationSection>

      <DocumentationSection title="Choose the right control">
        <Text
          label="Use Checkbox for independent choices submitted together, Switch for a setting applied immediately, and Radio when exactly one option in a group may be selected."
          wrap
        />
      </DocumentationSection>

      <DocumentationSection title="API">
        <ApiReference rows={rows('Checkbox')} />
      </DocumentationSection>
    </ComponentDocument>
  );
}

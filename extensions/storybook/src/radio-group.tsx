import React from 'react';
import { Code, InlineMessage, Radio, RadioGroup, Text } from '@husklet/react';
import {
  ApiReference,
  ComponentDocument,
  DocumentationSection,
  FieldSpecimen,
  SpecimenGrid,
} from './component-document.js';
import { rows } from './editors.js';

type Channel = 'stable' | 'nightly';

export function RadioGroupWorkbench() {
  const [selected, setSelected] = React.useState<Channel>('stable');

  function choose(option: Channel, value: unknown) {
    if (value === null || Boolean(value)) setSelected(option);
  }

  return (
    <ComponentDocument
      name="Radio Group"
      summary="RadioGroup lays out authored Radio children and joins them into one exclusive native selection group."
    >
      <DocumentationSection title="Overview">
        <FieldSpecimen label="Release channel" helper="Preview is unavailable for this workspace">
          <RadioGroup gap={1} orientation="vertical">
            <Radio
              label="Stable"
              checked={selected === 'stable'}
              onToggle={(report) => choose('stable', report.value)}
            />
            <Radio label="Preview" checked={false} enabled={false} />
            <Radio
              label="Nightly"
              checked={selected === 'nightly'}
              onToggle={(report) => choose('nightly', report.value)}
            />
          </RadioGroup>
        </FieldSpecimen>
        <InlineMessage label={`Selected channel: ${selected}`} tone="neutral" />
        <Code
          value={
            '<RadioGroup orientation="vertical">\n  <Radio label="Stable" checked={channel === "stable"} onToggle={chooseStable} />\n  <Radio label="Nightly" checked={channel === "nightly"} onToggle={chooseNightly} />\n</RadioGroup>'
          }
          wrap
        />
      </DocumentationSection>

      <DocumentationSection title="Layout">
        <SpecimenGrid>
          <FieldSpecimen label="Vertical" helper="Best for labels that need scanning">
            <RadioGroup orientation="vertical" gap={1}>
              <Radio label="Compact" checked />
              <Radio label="Comfortable" checked={false} />
            </RadioGroup>
          </FieldSpecimen>
          <FieldSpecimen label="Horizontal" helper="Use only for short, closely related labels">
            <RadioGroup orientation="horizontal" gap={3}>
              <Radio label="Light" checked={false} />
              <Radio label="Dark" checked />
            </RadioGroup>
          </FieldSpecimen>
        </SpecimenGrid>
      </DocumentationSection>

      <DocumentationSection title="Ownership and events">
        <Text
          label="RadioGroup owns grouping, orientation, and spacing. Authored Radio children own labels, checked state, enabled state, and onToggle events. Keep selection in one parent state so exactly one enabled child is checked."
          wrap
        />
      </DocumentationSection>

      <DocumentationSection title="Keyboard and accessibility">
        <Text
          label="Tab enters the group at one selected option. Arrow movement follows the native group, skips disabled children, and focuses and selects exactly one enabled option."
          wrap
        />
      </DocumentationSection>

      <DocumentationSection title="Inputs">
        <Text
          label="Author Radio children explicitly. RadioGroup does not accept choices or a group-level value/event: generated options could not carry controlled checked state, disabled behavior, or handlers."
          wrap
        />
      </DocumentationSection>

      <DocumentationSection title="API">
        <ApiReference rows={rows('RadioGroup')} />
      </DocumentationSection>
    </ComponentDocument>
  );
}

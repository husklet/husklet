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

type Shell = 'zsh' | 'bash' | 'fish';

export function RadioWorkbench() {
  const [selected, setSelected] = React.useState<Shell>('zsh');

  function choose(option: Shell, value: unknown) {
    if (value === null || Boolean(value)) setSelected(option);
  }

  return (
    <ComponentDocument
      name="Radio"
      summary="Radio chooses exactly one option from a visible group. Each labeled option is one native click target."
    >
      <DocumentationSection title="Overview">
        <RadioGroup gap={1} tooltip="Default shell" width={{ chars: 32 }}>
          <Radio
            label="Z shell"
            checked={selected === 'zsh'}
            onToggle={(report) => choose('zsh', report.value)}
          />
          <Radio
            label="Bash"
            checked={selected === 'bash'}
            onToggle={(report) => choose('bash', report.value)}
          />
          <Radio
            label="Fish"
            checked={selected === 'fish'}
            onToggle={(report) => choose('fish', report.value)}
          />
        </RadioGroup>
        <InlineMessage label={`Selected shell: ${selected}`} tone="neutral" />
        <Code
          value={
            '<RadioGroup>\n  <Radio label="Z shell" checked={shell === "zsh"} onToggle={() => setShell("zsh")} />\n  …\n</RadioGroup>'
          }
          wrap
        />
      </DocumentationSection>

      <DocumentationSection title="States">
        <SpecimenGrid>
          <FieldSpecimen label="Enabled group" helper="Exactly one option is selected">
            <RadioGroup gap={1} width={{ chars: 28 }}>
              <Radio label="Automatic" checked />
              <Radio label="Manual" checked={false} />
            </RadioGroup>
          </FieldSpecimen>
          <FieldSpecimen
            label="Disabled group"
            helper="Unavailable choices retain their known state"
          >
            <RadioGroup gap={1} width={{ chars: 28 }}>
              <Radio label="Managed · selected" checked enabled={false} />
              <Radio label="Custom · unselected" checked={false} enabled={false} />
            </RadioGroup>
          </FieldSpecimen>
        </SpecimenGrid>
      </DocumentationSection>

      <DocumentationSection title="Keyboard and accessibility">
        <Text
          label="Tab enters the group once. Arrow keys move selection among enabled options; Space selects the focused option. Keep every option visibly labeled and preserve one selected value."
          wrap
        />
      </DocumentationSection>

      <DocumentationSection title="Checked and selected">
        <Text
          label="Use checked for controlled state. selected is a legacy alias read by the host like checked; omit it in new code and never provide both properties."
          wrap
        />
      </DocumentationSection>

      <DocumentationSection title="Choose the right control">
        <Text
          label="Use Radio when exactly one visible option is required. Use Checkbox for independent choices. A standalone Radio is invalid because it gives no group context or alternative."
          wrap
        />
      </DocumentationSection>

      <DocumentationSection title="API">
        <ApiReference rows={rows('Radio')} />
      </DocumentationSection>
    </ComponentDocument>
  );
}

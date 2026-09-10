import React from 'react';
import { Autocomplete, Code, Text } from '@husklet/react';
import {
  ApiReference,
  ComponentDocument,
  DocumentationSection,
  FieldSpecimen,
  SpecimenGrid,
} from './component-document.js';
import { rows } from './editors.js';

const runtimes = [
  { value: 'node-22', label: 'Node.js 22' },
  { value: 'python-313', label: 'Python 3.13' },
  { value: 'rust-stable', label: 'Rust stable' },
];

export function AutocompleteWorkbench() {
  const [feedback, setFeedback] = React.useState(
    'Open the list and type to narrow its fixed options.',
  );
  return (
    <ComponentDocument
      name="Autocomplete"
      summary="Autocomplete searches a fixed producer-owned option list and reports the selected row."
    >
      <DocumentationSection title="Overview">
        <FieldSpecimen label="Runtime" helper={feedback} width={{ chars: 30 }}>
          <Autocomplete
            choices={runtimes}
            width={{ chars: 30 }}
            tooltip="Runtime"
            onSelect={(report) => {
              const index = Number(report.rows?.[0] ?? 0);
              setFeedback(`Selected ${runtimes[index]?.label ?? 'unknown option'}.`);
            }}
          />
        </FieldSpecimen>
        <Code value={'<Autocomplete choices={runtimes} onSelect={chooseRuntime} />'} wrap />
      </DocumentationSection>
      <DocumentationSection title="Options">
        <Text
          label="Each option has a stable machine value and a human label. Keep the list bounded and order the most useful matches first."
          wrap
        />
        <FieldSpecimen
          label="Short fixed list"
          helper="Typing filters labels; it does not create a new value"
          width={{ chars: 30 }}
        >
          <Autocomplete choices={runtimes} width={{ chars: 30 }} />
        </FieldSpecimen>
      </DocumentationSection>
      <DocumentationSection title="States">
        <SpecimenGrid>
          <FieldSpecimen
            label="Empty"
            helper="Explain why no options are available"
            width={{ chars: 30 }}
          >
            <Autocomplete choices={[]} width={{ chars: 30 }} />
          </FieldSpecimen>
          <FieldSpecimen
            label="Disabled"
            helper="The option list cannot be opened"
            width={{ chars: 30 }}
          >
            <Autocomplete choices={runtimes} enabled={false} width={{ chars: 30 }} />
          </FieldSpecimen>
        </SpecimenGrid>
      </DocumentationSection>
      <DocumentationSection title="Behavior">
        <Text
          label="Autocomplete accepts only declared choices. Keyboard typing searches labels, arrow keys move through matches, Enter selects, and Escape closes without inventing a value."
          wrap
        />
      </DocumentationSection>
      <DocumentationSection title="Accessibility">
        <Text
          label="Provide a persistent visible label, use distinct option labels, and keep selected-row feedback adjacent. Disabled and empty states need visible explanations."
          wrap
        />
      </DocumentationSection>
      <DocumentationSection title="API">
        <ApiReference rows={rows('Autocomplete')} />
      </DocumentationSection>
    </ComponentDocument>
  );
}

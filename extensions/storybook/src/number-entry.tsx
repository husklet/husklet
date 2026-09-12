import React from 'react';
import { Code, NumberEntry, Text } from '@husklet/react';
import {
  ApiReference,
  ComponentDocument,
  DocumentationSection,
  FieldSpecimen,
  SpecimenGrid,
} from './component-document.js';
import { rows } from './editors.js';

export function NumberEntryWorkbench() {
  const [state, setState] = React.useState({
    value: 4,
    feedback: 'Use the buttons or arrow keys.',
  });
  return (
    <ComponentDocument
      name="NumberEntry"
      summary="NumberEntry captures a bounded numeric value with native keyboard and step controls."
    >
      <DocumentationSection title="Overview">
        <FieldSpecimen label="Build workers" helper={state.feedback} width={{ chars: 14 }}>
          <NumberEntry
            value={state.value}
            minimum={1}
            maximum={16}
            step={1}
            width={{ chars: 14 }}
            tooltip="Build workers"
            onChange={(report) => {
              const value = Number(report.value);
              setState({ value, feedback: `${value} concurrent workers` });
            }}
          />
        </FieldSpecimen>
        <Code value="<NumberEntry value={workers} minimum={1} maximum={16} step={1} />" wrap />
      </DocumentationSection>

      <DocumentationSection title="Bounds">
        <SpecimenGrid>
          <FieldSpecimen
            label="Minimum · 1"
            helper="Cannot decrement below the declared minimum"
            width={{ chars: 14 }}
          >
            <NumberEntry value={1} minimum={1} maximum={16} step={1} width={{ chars: 14 }} />
          </FieldSpecimen>
          <FieldSpecimen
            label="Maximum · 16"
            helper="Cannot increment above the declared maximum"
            width={{ chars: 14 }}
          >
            <NumberEntry value={16} minimum={1} maximum={16} step={1} width={{ chars: 14 }} />
          </FieldSpecimen>
        </SpecimenGrid>
      </DocumentationSection>

      <DocumentationSection title="Steps">
        <SpecimenGrid>
          <FieldSpecimen label="Whole numbers · 1" width={{ chars: 14 }}>
            <NumberEntry value={4} minimum={0} maximum={10} step={1} width={{ chars: 14 }} />
          </FieldSpecimen>
          <FieldSpecimen label="Fractional · 0.25" width={{ chars: 14 }}>
            <NumberEntry value={1.5} minimum={0} maximum={4} step={0.25} width={{ chars: 14 }} />
          </FieldSpecimen>
        </SpecimenGrid>
      </DocumentationSection>

      <DocumentationSection title="States">
        <SpecimenGrid>
          <FieldSpecimen label="Enabled" width={{ chars: 14 }}>
            <NumberEntry value={8} minimum={1} maximum={16} width={{ chars: 14 }} />
          </FieldSpecimen>
          <FieldSpecimen label="Invalid" helper="Enter 2 to 16 workers" width={{ chars: 14 }}>
            <NumberEntry value={1} minimum={2} maximum={16} tone="danger" width={{ chars: 14 }} />
          </FieldSpecimen>
          <FieldSpecimen
            label="Disabled"
            helper="Keep the retained value visible"
            width={{ chars: 14 }}
          >
            <NumberEntry value={8} minimum={1} maximum={16} enabled={false} width={{ chars: 14 }} />
          </FieldSpecimen>
        </SpecimenGrid>
      </DocumentationSection>

      <DocumentationSection title="Behavior">
        <Text
          label="Up and Down move by step while focused. The native control clamps at minimum and maximum and reports the numeric value, not its formatted text."
          wrap
        />
      </DocumentationSection>
      <DocumentationSection title="Accessibility">
        <Text
          label="Pair the control with a persistent label that names the quantity and state units in nearby helper text. Do not encode units inside the editable numeric value."
          wrap
        />
      </DocumentationSection>
      <DocumentationSection title="API">
        <ApiReference rows={rows('NumberEntry')} />
      </DocumentationSection>
    </ComponentDocument>
  );
}

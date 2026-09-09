import React from 'react';
import { Checkbox, Code, Column, InlineMessage, Text } from '@husklet/react';
import {
  ApiReference,
  ComponentDocument,
  DocumentationSection,
  FieldSpecimen,
  SpecimenGrid,
} from './component-document.js';
import { apiRows } from './editors.js';

const OPTIONS = ['Logs', 'Metrics', 'Traces'];

export function CheckboxWorkbench() {
  const [selected, setSelected] = React.useState(() => new Set(['Logs']));
  const all = selected.size === OPTIONS.length;
  const mixed = selected.size > 0 && !all;

  function toggleAll() {
    setSelected(all ? new Set() : new Set(OPTIONS));
  }

  function toggleOption(option: string, value: unknown) {
    setSelected((current) => {
      const next = new Set(current);
      if (value) next.add(option);
      else next.delete(option);
      return next;
    });
  }

  return (
    <ComponentDocument
      name="Checkbox"
      summary="Checkbox represents an independent choice; indeterminate communicates a partially selected group."
    >
      <DocumentationSection title="Overview">
        <Column gap={1}>
          <Checkbox
            label={`Select all · ${selected.size} of ${OPTIONS.length}`}
            checked={all}
            indeterminate={mixed}
            tooltip="Select all diagnostics"
            onToggle={toggleAll}
          />
          {OPTIONS.map((option) => (
            <Checkbox
              key={option}
              label={option}
              checked={selected.has(option)}
              onToggle={(report) => toggleOption(option, report.value)}
            />
          ))}
          <InlineMessage
            label={
              all
                ? 'All diagnostics selected.'
                : mixed
                  ? 'Some diagnostics selected.'
                  : 'No diagnostics selected.'
            }
            tone="neutral"
          />
        </Column>
        <Code
          value="<Checkbox checked={all} indeterminate={some && !all} onToggle={selectAll} />"
          wrap
        />
      </DocumentationSection>

      <DocumentationSection title="State matrix">
        <SpecimenGrid>
          {[
            ['Enabled · unchecked', false, false, true],
            ['Enabled · checked', true, false, true],
            ['Enabled · mixed', false, true, true],
            ['Disabled · unchecked', false, false, false],
            ['Disabled · checked', true, false, false],
            ['Disabled · mixed', false, true, false],
          ].map(([label, checked, indeterminate, enabled]) => (
            <FieldSpecimen key={String(label)} label={String(label)}>
              <Checkbox
                label="Diagnostics"
                checked={Boolean(checked)}
                indeterminate={Boolean(indeterminate)}
                enabled={Boolean(enabled)}
              />
            </FieldSpecimen>
          ))}
        </SpecimenGrid>
      </DocumentationSection>

      <DocumentationSection title="Interaction and accessibility">
        <Text
          label="Mixed is a presentation state, not a third submitted value. Clicking the label or pressing Space on a mixed parent resolves it to checked; the next activation clears it. Keep native focus visible and announce the selected count in the label."
          wrap
        />
      </DocumentationSection>
      <DocumentationSection title="Choose the right control">
        <Text
          label="Use Checkbox for independent choices, Switch for an immediately applied setting, and Radio for exactly one choice in a group."
          wrap
        />
      </DocumentationSection>
      <DocumentationSection title="API">
        <ApiReference rows={apiRows('Checkbox')} />
      </DocumentationSection>
    </ComponentDocument>
  );
}

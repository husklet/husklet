import React from 'react';
import {
  Code,
  Entry,
  FormControl,
  FormHelperText,
  FormLabel,
  InlineMessage,
  Text,
} from '@husklet/react';
import {
  ApiReference,
  ComponentDocument,
  DocumentationSection,
  SpecimenGrid,
} from './component-document.js';
import { rows } from './editors.js';

export function FormControlWorkbench() {
  const [name, setName] = React.useState('workspace-tools');

  return (
    <ComponentDocument
      name="Form Control"
      summary="FormControl stacks one field with the label and helper text that identify and explain it."
    >
      <DocumentationSection title="Overview">
        <FormControl gap={1} width={{ chars: 38 }}>
          <FormLabel label="Extension name" />
          <Entry
            value={name}
            placeholder="my-extension"
            tooltip="Extension name"
            onChange={(report) => setName(String(report.value ?? ''))}
          />
          <FormHelperText label="Used in manifests and package names." />
        </FormControl>
        <InlineMessage label={`Current name: ${name || 'empty'}`} tone="neutral" />
        <Code
          value={
            '<FormControl>\n  <FormLabel label="Extension name" />\n  <Entry value={name} onChange={setName} />\n  <FormHelperText label="Used in manifests." />\n</FormControl>'
          }
          wrap
        />
      </DocumentationSection>

      <DocumentationSection title="States">
        <SpecimenGrid>
          <FormControl gap={1}>
            <FormLabel label="Normal" />
            <Entry value="workspace-tools" />
          </FormControl>
          <FormControl gap={1}>
            <FormLabel label="Required *" />
            <Entry value="" placeholder="Required value" />
            <FormHelperText label="Required fields need a visible text marker." />
          </FormControl>
          <FormControl gap={1}>
            <FormLabel label="Error" tone="danger" />
            <Entry value="Bad Name" tone="danger" />
            <FormHelperText label="Use lowercase letters, numbers, and hyphens." tone="danger" />
          </FormControl>
          <FormControl gap={1}>
            <FormLabel label="Disabled" />
            <Entry value="managed-extension" enabled={false} />
            <FormHelperText label="Managed by workspace policy." />
          </FormControl>
          <FormControl gap={1}>
            <FormLabel label="With helper text" />
            <Entry value="top" />
            <FormHelperText label="Keep guidance concise and specific." />
          </FormControl>
        </SpecimenGrid>
      </DocumentationSection>

      <DocumentationSection title="Ownership">
        <Text
          label="Entry owns value, enabled state, and onChange. FormControl owns only vertical validation layout; FormLabel and FormHelperText own the visible description and tone."
          wrap
        />
      </DocumentationSection>
      <DocumentationSection title="Accessibility">
        <Text
          label="The host associates FormLabel as the field’s accessible label and FormHelperText as its description. Keep both inside the same FormControl and place the label before the Entry."
          wrap
        />
      </DocumentationSection>
      <DocumentationSection title="API">
        <ApiReference rows={rows('FormControl')} />
      </DocumentationSection>
    </ComponentDocument>
  );
}

import React from 'react';
import { Code, PasswordEntry, Text } from '@husklet/react';
import {
  ApiReference,
  ComponentDocument,
  DocumentationSection,
  FieldSpecimen,
  SpecimenGrid,
} from './component-document.js';
import { rows } from './editors.js';

export function PasswordEntryWorkbench() {
  const [state, setState] = React.useState({
    value: 'local-token',
    feedback: 'Value is concealed.',
  });
  return (
    <ComponentDocument
      name="PasswordEntry"
      summary="PasswordEntry captures sensitive single-line text without displaying its value. Reveal access is an explicit product decision."
    >
      <DocumentationSection title="Overview">
        <FieldSpecimen label="Registry token" helper={state.feedback} width={{ chars: 30 }}>
          <PasswordEntry
            value={state.value}
            placeholder="Paste token"
            tooltip="Registry token"
            width={{ chars: 30 }}
            secret={false}
            onChange={(report) => {
              const value = String(report.value ?? '').slice(0, 512);
              setState({
                value,
                feedback: value ? `${value.length} characters · concealed` : 'Token is required.',
              });
            }}
          />
        </FieldSpecimen>
        <Code value={'<PasswordEntry value={token} secret={false} onChange={setToken} />'} wrap />
      </DocumentationSection>

      <DocumentationSection title="Reveal policy">
        <SpecimenGrid>
          <FieldSpecimen
            label="Peek available"
            helper="Use when a person may verify the value"
            width={{ chars: 30 }}
          >
            <PasswordEntry value="verify-me" secret={false} width={{ chars: 30 }} />
          </FieldSpecimen>
          <FieldSpecimen
            label="Peek withheld"
            helper="Use on shared or higher-risk surfaces"
            width={{ chars: 30 }}
          >
            <PasswordEntry value="never-reveal" secret width={{ chars: 30 }} />
          </FieldSpecimen>
        </SpecimenGrid>
      </DocumentationSection>

      <DocumentationSection title="States">
        <SpecimenGrid>
          <FieldSpecimen
            label="Empty"
            helper="Placeholder describes the expected credential"
            width={{ chars: 30 }}
          >
            <PasswordEntry value="" placeholder="Paste access token" width={{ chars: 30 }} />
          </FieldSpecimen>
          <FieldSpecimen
            label="Disabled"
            helper="A retained secret stays concealed"
            width={{ chars: 30 }}
          >
            <PasswordEntry value="stored-token" enabled={false} secret width={{ chars: 30 }} />
          </FieldSpecimen>
        </SpecimenGrid>
      </DocumentationSection>

      <DocumentationSection title="Behavior">
        <Text
          label="PasswordEntry is always concealed. secret controls whether the native peek affordance is withheld; it does not turn the component into plain text. Bound retained values and clear them when their workflow closes."
          wrap
        />
      </DocumentationSection>
      <DocumentationSection title="Accessibility">
        <Text
          label="Provide a persistent label naming the credential. Explain requirements without echoing the value, preserve native editing shortcuts, and never place secret content in helper text, diagnostics, or event receipts."
          wrap
        />
      </DocumentationSection>
      <DocumentationSection title="API">
        <ApiReference rows={rows('PasswordEntry')} />
      </DocumentationSection>
    </ComponentDocument>
  );
}

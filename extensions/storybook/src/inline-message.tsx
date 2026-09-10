import React from 'react';
import { Code, InlineMessage, Text } from '@husklet/react';
import { enums } from './catalogue.js';
import {
  ApiReference,
  ComponentDocument,
  DocumentationSection,
  FieldSpecimen,
  SpecimenGrid,
} from './component-document.js';
import { rows } from './editors.js';

type Tone = 'neutral' | 'accent' | 'positive' | 'warning' | 'danger';

const examples: Record<Tone, { label: string; use: string }> = {
  neutral: { label: 'No changes to apply.', use: 'Routine context without urgency.' },
  accent: { label: 'A newer image is available.', use: 'New or highlighted information.' },
  positive: { label: 'Workspace settings saved.', use: 'A completed operation.' },
  warning: { label: 'Two containers will restart.', use: 'A recoverable risk or caution.' },
  danger: { label: 'Network inventory is unavailable.', use: 'A failure requiring attention.' },
};

const tones = enums.Tone.map((member) => member.style as Tone);

export function InlineMessageWorkbench() {
  return (
    <ComponentDocument
      name="Inline Message"
      summary="InlineMessage communicates a concise result or condition beside the content it affects, using tone, text, and a non-color status cue together."
    >
      <DocumentationSection title="Overview">
        <InlineMessage label="Workspace settings saved." tone="positive" width="fill" />
        <Code value={'<InlineMessage label="Workspace settings saved." tone="positive" />'} wrap />
      </DocumentationSection>

      <DocumentationSection title="Tones">
        <SpecimenGrid>
          {tones.map((tone) => (
            <FieldSpecimen key={tone} label={tone} helper={examples[tone].use}>
              <InlineMessage label={examples[tone].label} tone={tone} width="fill" />
            </FieldSpecimen>
          ))}
        </SpecimenGrid>
        <Text
          label="Tone never carries meaning alone: every default cue has a distinct silhouette and the message states the condition in words."
          color="text-dim"
          wrap
        />
      </DocumentationSection>

      <DocumentationSection title="Wrapping">
        <FieldSpecimen
          label="Bounded diagnostic"
          helper="Keep the cause concise and place recovery actions after the message."
          width={{ chars: 44 }}
        >
          <InlineMessage
            label="Network inventory is unavailable. Check that the workspace is running, then retry."
            tone="danger"
            width="fill"
          />
        </FieldSpecimen>
        <FieldSpecimen
          label="Explicit icon override"
          helper="Use a known product icon only when it communicates more specifically than the tone default."
        >
          <InlineMessage
            label="Connected through the workspace network."
            tone="positive"
            icon="network-workgroup-symbolic"
            width="fill"
          />
        </FieldSpecimen>
      </DocumentationSection>

      <DocumentationSection title="Accessibility">
        <Text
          label="Inline messages expose alert semantics. Their decorative icon is not a separate focus stop; write the full outcome in the visible label and do not identify state only by color."
          wrap
        />
      </DocumentationSection>

      <DocumentationSection title="API">
        <ApiReference rows={rows('InlineMessage')} />
      </DocumentationSection>
    </ComponentDocument>
  );
}

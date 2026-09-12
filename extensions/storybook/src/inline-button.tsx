import React from 'react';
import { Code, Column, InlineButton, InlineMessage, Row, Text } from '@husklet/react';
import {
  ApiReference,
  ComponentDocument,
  DocumentationSection,
  SpecimenGrid,
} from './component-document.js';
import { rows } from './editors.js';

export function InlineButtonWorkbench() {
  const [event, setEvent] = React.useState('No interaction yet.');
  return (
    <ComponentDocument
      name="InlineButton"
      summary="Compact row-action chrome inside a full 44px pointer and keyboard target."
      contentWidth={{ maximum: { chars: 64 } }}
    >
      <DocumentationSection title="Overview">
        <Row gap={2} wrap align="center">
          <InlineButton
            label="Inspect resource"
            variant="outline"
            onInvoke={() => setEvent('Inspect resource invoked')}
            onFocus={() => setEvent('Inspect resource focused')}
            onKey={() => setEvent('Keyboard input received')}
          />
          <InlineMessage label={event} tone="neutral" />
        </Row>
        <Code
          width="fill"
          value={'<InlineButton label="Inspect resource" variant="outline" onInvoke={inspect} />'}
          wrap
        />
        <Text
          label="The visible chrome is 28px high; the native hit target remains at least 44px. Use it inside dense resource rows, not as a page’s primary action."
          color="text-dim"
          wrap
        />
      </DocumentationSection>
      <SpecimenGrid>
        <DocumentationSection title="Variants">
          <Row gap={2} wrap align="center">
            <InlineButton label="Outline" variant="outline" />
            <InlineButton label="Active" variant="filled" tone="accent" />
            <InlineButton label="Ghost" variant="ghost" />
            <InlineButton label="Plain" variant="plain" />
          </Row>
        </DocumentationSection>
        <DocumentationSection title="States">
          <Row gap={2} wrap align="center">
            <InlineButton label="Available" variant="outline" />
            <InlineButton
              label="Focused action"
              variant="outline"
              onFocus={() => setEvent('Focus visible')}
            />
            <InlineButton label="Unavailable" variant="outline" enabled={false} />
            <InlineButton label="Reading…" variant="outline" busy />
          </Row>
        </DocumentationSection>
      </SpecimenGrid>
      <DocumentationSection title="Placement">
        <Column gap={1} width="fill">
          <Text label="alpine:3.20 · 7.8 MiB" />
          <Row gap={1} wrap align="center">
            <InlineButton label="Inspect" variant="outline" />
            <InlineButton label="Hide details" variant="filled" tone="accent" />
          </Row>
        </Column>
      </DocumentationSection>
      <DocumentationSection title="Accessibility">
        <Text
          label="Use a specific verb phrase. Every enabled action participates in keyboard focus order and reports Invoke, Focus, Key, Pointer, and Context interactions."
          wrap
        />
      </DocumentationSection>
      <DocumentationSection title="API">
        <ApiReference rows={rows('InlineButton')} />
      </DocumentationSection>
    </ComponentDocument>
  );
}

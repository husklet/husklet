import React from 'react';
import { Code, Column, Expander, InlineMessage, Text } from '@husklet/react';
import {
  ApiReference,
  ComponentDocument,
  DocumentationSection,
  FieldSpecimen,
  SpecimenGrid,
} from './component-document.js';
import { rows } from './editors.js';

export function ExpanderWorkbench() {
  const [expanded, setExpanded] = React.useState(false);
  const [events, setEvents] = React.useState(0);

  function change(value: unknown) {
    const next = Boolean(value);
    setExpanded(next);
    setEvents((current) => current + 1);
  }

  return (
    <ComponentDocument
      name="Expander"
      summary="Expander progressively discloses supporting content while its labelled header remains available in the page's reading and keyboard order."
    >
      <DocumentationSection title="Overview">
        <Expander
          label="Runtime diagnostics"
          expanded={expanded}
          onExpand={(report) => change(report.value)}
          width="fill"
        >
          <Column gap={2} pad={{ top: 1, start: 4 }} width="fill">
            <Text label="Socket connected" />
            <Text label="Last inspected 12 seconds ago" color="text-dim" />
          </Column>
        </Expander>
        <InlineMessage
          label={
            events === 0
              ? 'No disclosure change yet.'
              : `Runtime diagnostics ${expanded ? 'expanded' : 'collapsed'} · ${events} ${events === 1 ? 'event' : 'events'}`
          }
          tone="neutral"
        />
        <Code
          value={`<Expander label="Runtime diagnostics" expanded={${expanded}} onExpand={setExpanded}>…</Expander>`}
          wrap
        />
      </DocumentationSection>

      <DocumentationSection title="API">
        <ApiReference rows={rows('Expander')} />
      </DocumentationSection>

      <DocumentationSection title="States">
        <SpecimenGrid>
          <FieldSpecimen label="Collapsed" helper="Only the persistent summary is visible.">
            <Expander label="Technical details" expanded={false} width="fill">
              <Text label="Hidden until requested" />
            </Expander>
          </FieldSpecimen>
          <FieldSpecimen
            label="Expanded"
            helper="Related details remain grouped beneath the same summary."
          >
            <Expander label="Technical details" expanded width="fill">
              <Column gap={1} pad={{ top: 1, start: 4 }} width="fill">
                <Text label="Request ID · req-2841" />
                <Text label="Transport · local socket" color="text-dim" />
              </Column>
            </Expander>
          </FieldSpecimen>
        </SpecimenGrid>
      </DocumentationSection>

      <DocumentationSection title="Action disclosure">
        <Text
          label="Use the outline variant when a compact disclosure sits beside immediate actions. Its bounded 28px summary reads as a control without turning the disclosed content into a bordered panel."
          color="text-dim"
          wrap
        />
        <Expander
          label="More actions"
          expanded={false}
          variant="outline"
          width="content"
          align="start"
          tooltip="Show secondary actions"
        >
          <Text label="Secondary actions appear here." />
        </Expander>
      </DocumentationSection>

      <DocumentationSection title="Behavior">
        <Text
          label="Keep the summary short and stable as the disclosure changes. Enter or Space toggles the focused summary; onExpand reports the resulting state once so a controlled Expander can render it back."
          wrap
        />
        <FieldSpecimen
          label="Bounded narrow content"
          helper="The summary and body remain inside their parent at compact widths."
        >
          <Expander
            label="Connection diagnostics and immutable resource details"
            expanded
            width="fill"
          >
            <Text
              label="Long diagnostic values wrap inside the disclosed body instead of widening the surrounding page."
              wrap
              width="fill"
            />
          </Expander>
        </FieldSpecimen>
      </DocumentationSection>

      <DocumentationSection title="Accessibility">
        <Text
          label="The visible summary is the disclosure's accessible name and exposes whether its body is expanded. Keep focus on the summary after toggling and do not hide a primary action inside a disclosure."
          wrap
        />
      </DocumentationSection>
    </ComponentDocument>
  );
}

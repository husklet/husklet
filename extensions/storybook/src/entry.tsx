import React from 'react';
import { Column, Entry, Expander, Row, Select, Switch, Text } from '@husklet/react';
import {
  ApiReference,
  ComponentDocument,
  DocumentationSection,
  FieldSpecimen,
  choices,
} from './component-document.js';
import { rows } from './editors.js';

type Width = 'compact' | 'default' | 'wide';
type Tone = 'neutral' | 'danger';

const widths = { compact: { chars: 18 }, default: { chars: 30 }, wide: { chars: 48 } } as const;

export function EntryWorkbench() {
  const [value, setValue] = React.useState('workspace-api');
  const [width, setWidth] = React.useState<Width>('default');
  const [tone, setTone] = React.useState<Tone>('neutral');
  const [enabled, setEnabled] = React.useState(true);
  const [secret, setSecret] = React.useState(false);
  const [event, setEvent] = React.useState('Edit or submit the field to inspect its event.');
  return (
    <ComponentDocument
      name="Entry"
      summary="Entry captures one short line of text. Give it a visible purpose, a useful empty-state hint, and immediate validation feedback."
    >
      <DocumentationSection title="Overview">
        <FieldSpecimen label="Extension name" helper={event} width={widths[width]}>
          <Entry
            value={value}
            width={widths[width]}
            align="start"
            tone={tone}
            enabled={enabled}
            secret={secret}
            placeholder="Extension name"
            tooltip="Extension name"
            onChange={(report) => {
              const next = String(report.value ?? '').slice(0, 64);
              setValue(next);
              setEvent(`Changed · ${next.length} characters`);
            }}
            onSubmit={() => setEvent(`Submitted · ${value || 'empty value'}`)}
          />
        </FieldSpecimen>
      </DocumentationSection>
      <DocumentationSection title="Widths">
        <Column gap={2}>
          <FieldSpecimen label="Compact · 18ch" width={{ chars: 18 }}>
            <Entry value="workspace-api" width={{ chars: 18 }} />
          </FieldSpecimen>
          <FieldSpecimen label="Default · 30ch" width={{ chars: 30 }}>
            <Entry value="workspace-api" width={{ chars: 30 }} />
          </FieldSpecimen>
          <FieldSpecimen label="Wide · 48ch" width={{ chars: 48 }}>
            <Entry value="workspace-api" width={{ chars: 48 }} />
          </FieldSpecimen>
        </Column>
        <Text
          label="Choose width from expected content, not from the current value."
          color="text-dim"
        />
      </DocumentationSection>
      <DocumentationSection title="States">
        <Column gap={2}>
          <FieldSpecimen label="Default" helper="Ready to edit" width={{ chars: 36 }}>
            <Entry value="workspace-api" width={{ chars: 36 }} />
          </FieldSpecimen>
          <FieldSpecimen
            label="Error"
            helper="Use lowercase letters, digits, and hyphens"
            width={{ chars: 36 }}
          >
            <Entry value="Workspace API" tone="danger" width={{ chars: 36 }} />
          </FieldSpecimen>
          <FieldSpecimen
            label="Disabled"
            helper="Available after workspace setup"
            width={{ chars: 36 }}
          >
            <Entry value="workspace-api" enabled={false} width={{ chars: 36 }} />
          </FieldSpecimen>
          <FieldSpecimen label="Secret" helper="Value remains concealed" width={{ chars: 36 }}>
            <Entry value="token-value" secret width={{ chars: 36 }} />
          </FieldSpecimen>
        </Column>
      </DocumentationSection>
      <DocumentationSection title="Accessibility">
        <Text
          label="Place a visible label beside the field in real forms. A placeholder is an example, not a replacement for a label. Explain disabled and invalid states in nearby text."
          wrap
        />
      </DocumentationSection>
      <Expander label="Playground" expanded={false} width="fill">
        <Column gap={2} pad={2}>
          <Row gap={2} wrap>
            <Select
              value={width}
              choices={choices(['compact', 'default', 'wide'])}
              tooltip="Field width"
              onChange={(report) => setWidth(report.value as Width)}
            />
            <Select
              value={tone}
              choices={choices(['neutral', 'danger'])}
              tooltip="Validation tone"
              onChange={(report) => setTone(report.value as Tone)}
            />
            <Switch
              checked={enabled}
              tooltip="Enabled"
              onToggle={(report) => setEnabled(Boolean(report.value))}
            />
            <Switch
              checked={secret}
              tooltip="Hide value"
              onToggle={(report) => setSecret(Boolean(report.value))}
            />
          </Row>
        </Column>
      </Expander>
      <DocumentationSection title="API">
        <ApiReference
          example={`<Entry value={name} placeholder="Extension name" onChange={updateName} />`}
          rows={rows('Entry')}
        />
      </DocumentationSection>
    </ComponentDocument>
  );
}

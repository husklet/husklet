import React from 'react';
import { Code, Column, Expander, Heading, Select, Text } from '@husklet/react';
import { enums } from './catalogue.js';
import {
  ApiReference,
  ComponentDocument,
  DocumentationSection,
  FieldSpecimen,
  SpecimenGrid,
} from './component-document.js';
import { rows } from './editors.js';

type Scale = 'caption' | 'body' | 'title' | 'display';

const scales = enums.Scale.map((scale) => ({
  ...scale,
  style: scale.style as Scale,
}));

export function HeadingWorkbench() {
  const [scale, setScale] = React.useState<Scale>('title');
  return (
    <ComponentDocument
      name="Heading"
      summary="Heading gives a section a semantic heading while Scale controls its visual place in the interface hierarchy."
    >
      <DocumentationSection title="Overview">
        <Code value={'<Heading label="Workspace settings" scale="title" />'} wrap />
        <Heading label="Workspace settings" scale="title" />
        <Text
          label="Use Heading for structure. Use Text when copy is not a section title."
          color="text-dim"
          wrap
        />
      </DocumentationSection>
      <DocumentationSection title="Type scale">
        <Column gap={3} width="fill">
          {scales.map((item) => (
            <Column key={item.style} gap={1} width="fill">
              <Text
                label={`${item.style} · ${item.pixels}px · weight ${item.weight}`}
                color="text-dim"
              />
              <Heading label="Build, inspect, and ship with confidence" scale={item.style} wrap />
            </Column>
          ))}
        </Column>
        <Text
          label="Scale is a typography role, not component height or surrounding spacing."
          color="text-dim"
          wrap
        />
      </DocumentationSection>
      <DocumentationSection title="Wrapping and truncation">
        <SpecimenGrid>
          <FieldSpecimen label="Wrap in bounded content" width={{ chars: 30 }}>
            <Heading
              label="A long workspace heading wraps without widening its page"
              scale="title"
              width={{ chars: 30 }}
              wrap
            />
          </FieldSpecimen>
          <FieldSpecimen
            label="Ellipsize only when the full title is available elsewhere"
            width={{ chars: 30 }}
          >
            <Heading
              label="A long immutable resource title that must remain on one line"
              scale="title"
              width={{ chars: 30 }}
              ellipsize
            />
          </FieldSpecimen>
        </SpecimenGrid>
      </DocumentationSection>
      <DocumentationSection title="Accessibility">
        <Text
          label="Heading exposes native heading semantics. Keep visible headings descriptive and preserve their reading order; visual Scale does not replace document structure."
          wrap
        />
      </DocumentationSection>
      <DocumentationSection title="API">
        <ApiReference rows={rows('Heading')} />
      </DocumentationSection>
      <Expander label="Playground" expanded={false} width="fill">
        <FieldSpecimen label="Scale" width={{ chars: 30 }}>
          <Select
            width={{ chars: 20 }}
            value={scale}
            choices={scales.map((item) => ({ value: item.style, label: item.style }))}
            onChange={(event) => setScale(String(event.value ?? 'title') as Scale)}
          />
        </FieldSpecimen>
        <Heading label="Adjustable heading" scale={scale} wrap />
      </Expander>
    </ComponentDocument>
  );
}

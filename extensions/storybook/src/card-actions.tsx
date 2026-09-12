import React from 'react';
import { Button, CardActions, Code, Text } from '@husklet/react';

import {
  ApiReference,
  ComponentDocument,
  DocumentationSection,
  SpecimenGrid,
} from './component-document.js';
import { rows } from './editors.js';

function Actions({ justify }: { justify: 'start' | 'center' | 'end' }) {
  return (
    <CardActions
      width="fill"
      gap={1}
      align="center"
      justify={justify}
      tooltip={`Actions aligned ${justify}`}
    >
      <Button label="Details" size="small" tone="accent" />
      <Button label="Start" size="small" variant="outline" />
    </CardActions>
  );
}

export function CardActionsWorkbench() {
  return (
    <ComponentDocument
      name="CardActions"
      summary="CardActions keeps related commands in one compact, consistently aligned action row."
    >
      <DocumentationSection title="Overview">
        <Actions justify="start" />
        <Code
          value={'<CardActions gap={1} align="center">\n  <Button size="small" />\n</CardActions>'}
          wrap
        />
      </DocumentationSection>

      <DocumentationSection title="Alignment">
        <Text
          label="Start alignment is the default for operational cards. Center alignment suits a focused choice, while end alignment suits confirmation footers."
          color="text-dim"
          wrap
        />
        <SpecimenGrid>
          <Actions justify="start" />
          <Actions justify="center" />
          <Actions justify="end" />
        </SpecimenGrid>
      </DocumentationSection>

      <DocumentationSection title="Density">
        <Text
          label="Use the 28px small tier for repeated inventory actions. Align the row to center so siblings cannot stretch compact buttons to the height of a disclosure or status panel."
          color="text-dim"
          wrap
        />
        <CardActions width={{ chars: 28 }} gap={1} align="center" justify="start">
          <Button label="Inspect" size="small" />
          <Button label="Remove" size="small" tone="danger" variant="outline" />
        </CardActions>
      </DocumentationSection>

      <DocumentationSection title="Width constraints">
        <Text
          label="At narrow widths, keep command labels concise and preserve their source order. Move secondary actions into a disclosure before allowing an action row to overflow."
          color="text-dim"
          wrap
        />
        <CardActions width={{ chars: 26 }} gap={1} align="center" justify="start">
          <Button label="Review update" size="small" tone="accent" />
          <Button label="Check for changes" size="small" variant="ghost" />
        </CardActions>
      </DocumentationSection>

      <DocumentationSection title="Accessibility">
        <Text
          label="Keep the primary action first in source order, use visible verb labels, and preserve keyboard focus for every command."
          wrap
        />
      </DocumentationSection>

      <DocumentationSection title="API">
        <ApiReference rows={rows('CardActions')} />
      </DocumentationSection>
    </ComponentDocument>
  );
}

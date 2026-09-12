import React from 'react';
import {
  Button,
  Card,
  CardActions,
  CardContent,
  CardHeader,
  Code,
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

function ProjectCard({ variant }: { variant: 'outline' | 'filled' }) {
  return (
    <Card variant={variant} width="fill">
      <CardHeader
        icon="folder-symbolic"
        label="workspace-tools"
        detail="Rust · updated 3 minutes ago"
      />
      <CardContent>
        <Text label="Build and inspect the workspace extension from one bounded surface." wrap />
        <InlineMessage label="Checks passed" tone="positive" />
      </CardContent>
      <CardActions>
        <Button label="Open" size="small" />
        <Button label="More actions" size="small" variant="ghost" />
      </CardActions>
    </Card>
  );
}

export function CardWorkbench() {
  return (
    <ComponentDocument
      name="Card"
      summary="Card groups one concise subject, its supporting content, and related actions into a bounded surface."
    >
      <DocumentationSection title="Overview">
        <ProjectCard variant="outline" />
        <Code
          value={
            '<Card variant="outline">\n  <CardHeader label="workspace-tools" />\n  <CardContent>…</CardContent>\n  <CardActions>…</CardActions>\n</Card>'
          }
          wrap
        />
      </DocumentationSection>

      <DocumentationSection title="Anatomy">
        <Text
          label="Keep the order Header → Content → Actions. The header names one subject, content explains it, and actions affect only that subject."
          color="text-dim"
          wrap
        />
      </DocumentationSection>

      <DocumentationSection title="Variants">
        <SpecimenGrid>
          <ProjectCard variant="outline" />
          <ProjectCard variant="filled" />
        </SpecimenGrid>
      </DocumentationSection>

      <DocumentationSection title="Sizing">
        <Text
          label="Use an authored character width for compact peer cards. The outer surface keeps that width instead of inheriting the page's fill policy."
          color="text-dim"
          wrap
        />
        <Card variant="outline" width={{ chars: 32 }}>
          <CardHeader label="Compact card" detail="32ch" />
          <CardContent>
            <Text label="A bounded surface for short catalogue content." wrap />
          </CardContent>
          <CardActions>
            <Button label="Open" size="small" />
          </CardActions>
        </Card>
      </DocumentationSection>

      <DocumentationSection title="Inventory layout">
        <Text
          label="Use fill width for a single-column operational inventory. It preserves the page rhythm and leaves room for identity, state, and actions at narrow widths."
          color="text-dim"
          wrap
        />
        <Card variant="outline" width="fill">
          <CardHeader label="Inventory record" detail="alpine:3.20" />
          <CardContent>
            <Text label="Exited · ID aaaaaaaaaaaa" color="text-dim" />
          </CardContent>
          <CardActions>
            <Button label="Details" size="small" />
            <Button label="Start" size="small" variant="outline" />
          </CardActions>
        </Card>
      </DocumentationSection>

      <DocumentationSection title="Action hierarchy">
        <Text
          label="Keep identity and state separate from actions. Put the immediate next step first, then use a quiet explicit verb for maintenance; never make developers decode an unlabeled glyph."
          color="text-dim"
          wrap
        />
        <Card variant="outline" width={{ chars: 36 }}>
          <CardHeader label="extension-storybook" detail="Version 2.0.0 · Running" />
          <CardContent>
            <Text label="Update available · Version 2.1.0" color="text-dim" />
          </CardContent>
          <CardActions>
            <Button label="Review update" size="small" tone="accent" />
            <Button label="Check for changes" size="small" variant="ghost" />
          </CardActions>
        </Card>
      </DocumentationSection>

      <DocumentationSection title="Wrapping">
        <Card variant="outline" width="fill">
          <CardHeader
            icon="network-workgroup-symbolic"
            label="development-network-with-a-long-stable-name"
            detail="Local bridge shared by stopped workspace containers"
          />
          <CardContent>
            <Text
              label="Long identifiers and operational explanations wrap inside the surface without widening the page or hiding the actions that resolve the condition."
              wrap
            />
          </CardContent>
          <CardActions>
            <Button label="Inspect" size="small" />
            <Button label="Remove" size="small" tone="danger" variant="outline" />
          </CardActions>
        </Card>
      </DocumentationSection>

      <DocumentationSection title="Accessibility">
        <Text
          label="Start with a specific visible heading. Keep actions in source order after the content, use explicit action labels, and do not turn the whole card into a click target; CardActionArea owns that separate interaction."
          wrap
        />
      </DocumentationSection>

      <DocumentationSection title="API">
        <ApiReference rows={rows('Card')} />
      </DocumentationSection>
    </ComponentDocument>
  );
}

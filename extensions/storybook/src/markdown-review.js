import React from 'react';
import { Card, CardContent, Column, Heading, MarkdownView, Text } from '@husklet/react';

const { createElement: h } = React;

export const MARKDOWN_STORY = 'Review release notes';

const NOTES = `# Husklet 0.4

Review the upgrade before applying it.

## Changes
- Adds bounded semantic pane inspection
- Preserves terminal state during provider switches

> Extension-authored HTML remains inert.

\`\`\`
husklet extension inspect workspace-manager
\`\`\``;

/** A release/help document composed without a web view or executable markup. */
export function MarkdownReviewStory() {
  return h(
    Column,
    { gap: 2, grow: true },
    h(Heading, { label: 'Release-note review', scale: 'title' }),
    h(Text, { value: 'Selectable native text with headings, lists, quotes, and fenced code.' }),
    h(Card, {}, h(CardContent, {}, h(MarkdownView, { value: NOTES, grow: true }))),
  );
}

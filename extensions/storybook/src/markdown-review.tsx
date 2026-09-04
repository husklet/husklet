// @ts-nocheck -- legacy story typing is migrated incrementally.
import React from 'react';
import { Card, CardContent, Column, Heading, MarkdownView, Text } from '@husklet/react';


export const MARKDOWN_STORY = 'Review release notes';

const NOTES = `# Husklet 0.4

Review the upgrade before applying it.

## Changes
- Adds bounded semantic pane inspection
- Preserves terminal state during provider switches

> Extension-authored HTML remains inert.

\`\`\`
husklet extension inspect top
\`\`\``;

/** A release/help document composed without a web view or executable markup. */
export function MarkdownReviewStory() {
  return (
    <Column gap={2} grow={true}>
      <Heading label={'Release-note review'} scale={'title'} />
      <Text
        value={'Selectable native text with headings, lists, quotes, and fenced code.'} />
      <Card>
        <CardContent>
          <MarkdownView value={NOTES} grow={true} />
        </CardContent>
      </Card>
    </Column>
  );
}

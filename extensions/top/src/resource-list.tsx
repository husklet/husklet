import React from 'react';
import { Column, Row } from '@husklet/react';

const ITEM_WIDTH = {
  minimum: { chars: 34 },
} as const;

/**
 * A readable resource inventory at every allocation.
 *
 * Items share the available row on a wide page. A lone item grows to consume
 * that row; constraining its maximum would leave an island because GTK applies
 * the constraint after distributing the spare space. The host's wrapping
 * layout moves items onto their own rows as the pane narrows, without
 * duplicating or replacing the item tree.
 */
export function ResourceList({ children }: { children: React.ReactNode }) {
  return (
    <Row width="fill" gap={2} align="start" justify="start" wrap>
      {React.Children.toArray(children).map((child, index) => (
        <Column key={React.isValidElement(child) ? child.key : index} grow width={ITEM_WIDTH}>
          {child}
        </Column>
      ))}
    </Row>
  );
}

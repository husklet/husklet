import React from 'react';
import { Button, Column, InlineMessage } from '@husklet/react';

export function AuthorityRecovery({
  resource,
  onOpenExtensions,
}: {
  resource: 'container' | 'network';
  onOpenExtensions: () => void;
}) {
  return (
    <Column gap={1} align="start" width="fill">
      <InlineMessage
        label={`Top does not have permission to inspect this ${resource}. Review its exact ${resource} access in Extensions, then inspect again.`}
        tone="warning"
      />
      <Button label="Open Extensions" variant="filled" tone="accent" onInvoke={onOpenExtensions} />
    </Column>
  );
}

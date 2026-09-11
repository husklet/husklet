import React from 'react';
import { Button, InlineMessage } from '@husklet/react';

export function AuthorityRecovery({
  resource,
  onOpenExtensions,
}: {
  resource: 'container' | 'network' | 'volume';
  onOpenExtensions: () => void;
}) {
  const label =
    resource === 'volume'
      ? 'Volume access is denied. Review access in Extensions, then inspect again.'
      : `Top does not have permission to inspect this ${resource}. Review its exact ${resource} access in Extensions, then inspect again.`;
  return (
    <>
      <InlineMessage label={label} tone="warning" />
      <Button
        label="Review access"
        size="small"
        variant="filled"
        tone="accent"
        onInvoke={onOpenExtensions}
      />
    </>
  );
}

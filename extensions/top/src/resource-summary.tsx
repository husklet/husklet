import React from 'react';
import { CardContent, CardHeader, Row } from '@husklet/react';

type ResourceSummaryProps = {
  label: string;
  detail?: string;
  status?: React.ReactNode;
  actions: React.ReactNode;
  overflow?: React.ReactNode;
};

/** A compact, wrapping identity/status/action line for Top inventory cards. */
export function ResourceSummary({
  label,
  detail,
  status,
  actions,
  overflow,
}: ResourceSummaryProps) {
  return (
    <CardContent gap={1} align="center" width="fill">
      <Row gap={2} align="center" justify="start" wrap width="fill">
        <CardHeader label={label} detail={detail} align="start" grow width="fill" />
        <Row gap={1} align="center" justify="start" wrap>
          {status}
          {actions}
          {overflow}
        </Row>
      </Row>
    </CardContent>
  );
}

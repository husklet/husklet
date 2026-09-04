// @ts-nocheck -- legacy story typing is migrated incrementally.
import React from 'react';
import { Button, Column, Heading, InlineMessage, QueryPlan, QueryPlanMetric, QueryPlanNode, Row } from '@husklet/react';

const { useState } = React;

export const QUERY_PLAN_STORY = 'Query plan';
export const QUERY_PLAN_MODES = Object.freeze({ full: 'Full plan', hotspot: 'Hotspots', mismatch: 'Estimate mismatches' });

export const queryPlan = Object.freeze({
  id: 'root', operator: 'result', label: 'Customer activity report', relation: '', state: 'normal', detail: 'projection',
  metrics: { estimated_rows: 1200, actual_rows: 1184, cost: 840.2, duration_us: 12940, loops: 1 },
  children: [{
    id: 'join', operator: 'hash_join', label: 'Join customers to recent orders', relation: '', state: 'normal', detail: 'customer_id = id',
    metrics: { estimated_rows: 1200, actual_rows: 1184, cost: 810.4, duration_us: 12710, loops: 1 },
    children: [{
      id: 'customers', operator: 'index_scan', label: 'Customers by account', relation: 'customers_account_id_idx', state: 'normal', detail: 'account_id = $1',
      metrics: { estimated_rows: 1200, actual_rows: 1184, cost: 94.2, duration_us: 830, loops: 1 }, children: [],
    }, {
      id: 'orders-hash', operator: 'hash', label: 'Hash recent orders', relation: '', state: 'normal', detail: 'batches=8 memory=4 MiB',
      metrics: { estimated_rows: 24000, actual_rows: 289440, cost: 702.8, duration_us: 11620, loops: 1 },
      children: [{
        id: 'orders', operator: 'table_scan', label: 'Recent orders', relation: 'orders', state: 'hot', detail: 'filter created_at >= $2',
        metrics: { estimated_rows: 24000, actual_rows: 289440, cost: 690.1, duration_us: 10890, loops: 1 }, children: [],
      }],
    }],
  }, {
    id: 'preferences', operator: 'subquery_scan', label: 'Preference summary', relation: 'preferences', state: 'estimate_mismatch', detail: 'actual rows are 24× estimate',
    metrics: { estimated_rows: 50, actual_rows: 1184, cost: 29.8, duration_us: 230, loops: 1 }, children: [],
  }],
});

function selected(node, mode) {
  return mode === 'full' || (mode === 'hotspot' && node.state === 'hot') || (mode === 'mismatch' && node.state === 'estimate_mismatch');
}

/** Keeps matching operators and their complete root path, but no unrelated siblings. */
export function filterQueryPlan(node, mode) {
  const children = node.children.map((child) => filterQueryPlan(child, mode)).filter(Boolean);
  return selected(node, mode) || children.length > 0 ? { ...node, children } : null;
}

function count(node) { return 1 + node.children.reduce((total, child) => total + count(child), 0); }

function render(node) {
  return (
    <QueryPlanNode
      key={node.id}
      label={`${node.operator} · ${node.label}`}
      value={`id=${node.id} operator=${node.operator} state=${node.state} relation=${node.relation} detail=${node.detail}`}>
      {Object.entries(node.metrics).map(([label, value]) => <QueryPlanMetric key={label} label={label} value={value} />)}
      {node.children.map(render)}
    </QueryPlanNode>
  );
}

export function QueryPlanStory() {
  const [mode, setMode] = useState('full');
  const filtered = filterQueryPlan(queryPlan, mode);
  const shown = count(filtered);
  return (
    <Column gap={2}>
      <Heading label={'Query execution plan'} />
      <Row gap={1} wrap={true}>
        {Object.entries(QUERY_PLAN_MODES).map(([key, label]) => <Button
          key={key}
          label={label}
          enabled={mode !== key}
          onInvoke={() => setMode(key)} />)}
      </Row>
      <InlineMessage
        label={mode === 'full' ? 'Showing the complete captured plan.' : `Showing ${QUERY_PLAN_MODES[mode].toLowerCase()} with their ancestor paths.`} />
      <QueryPlan
        label={`${shown} plan operators`}
        detail={`bounded source: showing ${shown} of 84 operators`}>
        {render(filtered)}
      </QueryPlan>
    </Column>
  );
}

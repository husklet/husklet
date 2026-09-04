import React from 'react';
import { Button, Column, DependencyCycle, DependencyCycleMember, DependencyEdge, DependencyGraph, DependencyNode, Heading, InlineMessage, Row, Text } from '@husklet/react';

const { useState } = React;
export const DEPENDENCY_GRAPH_STORY = 'Dependency graph';
export const NODE_LIMIT = 32;
export const EDGE_LIMIT = 128;
export const CYCLE_LIMIT = 8;
export const MEMBER_LIMIT = 6;

type GraphNode = { id: string; label: string; version: string; state: string; detail: string };
type GraphEdge = { source: string; target: string; relation: string; requirement: string };
type GraphTotals = { nodes: number; edges: number; cycles: number };
type BoundedGraph = { nodes: GraphNode[]; edges: GraphEdge[]; cycles: string[][]; totals: GraphTotals };

const clean = (value: unknown, limit: number): string => String(value).replace(/[\u0000-\u001f\u007f-\u009f]/g, ' ').slice(0, limit);
function record(value: unknown): Record<string, unknown> | null {
  return value !== null && typeof value === 'object' && !Array.isArray(value) ? value as Record<string, unknown> : null;
}
function total(value: unknown, retained: number): number {
  return Number.isSafeInteger(value) && Number(value) >= retained ? Number(value) : retained;
}

export function boundedGraph(value: unknown): BoundedGraph {
  const graph = record(value);
  const rawNodes = graph && Array.isArray(graph.nodes) ? graph.nodes : [];
  const nodes: GraphNode[] = [];
  const ids = new Set<string>();
  for (const value of rawNodes) {
    if (nodes.length === NODE_LIMIT) break;
    const node = record(value);
    if (!node) continue;
    const id = clean(node.id, 40);
    if (!id.trim() || ids.has(id)) continue;
    ids.add(id);
    nodes.push({ id, label: clean(node.label, 120), version: clean(node.version, 64), state: clean(node.state, 40), detail: clean(node.detail, 160) });
  }
  const rawEdges = graph && Array.isArray(graph.edges) ? graph.edges : [];
  const edges: GraphEdge[] = [];
  for (const value of rawEdges) {
    if (edges.length === EDGE_LIMIT) break;
    const edge = record(value);
    if (!edge) continue;
    const source = clean(edge.source, 40);
    const target = clean(edge.target, 40);
    if (!ids.has(source) || !ids.has(target)) continue;
    edges.push({ source, target, relation: clean(edge.relation, 64), requirement: clean(edge.requirement, 120) });
  }
  const rawCycles = graph && Array.isArray(graph.cycles) ? graph.cycles : [];
  const cycles = rawCycles.filter((value): value is string[] => Array.isArray(value)
    && value.length > 0 && value.length <= MEMBER_LIMIT
    && value.every((id) => typeof id === 'string' && ids.has(id))
    && value.every((id, index) => edges.some((edge) => edge.source === id && edge.target === value[(index + 1) % value.length])))
    .slice(0, CYCLE_LIMIT).map((cycle) => [...cycle]);
  const totals = record(graph?.totals);
  return { nodes, edges, cycles, totals: {
    nodes: total(totals?.nodes, nodes.length), edges: total(totals?.edges, edges.length), cycles: total(totals?.cycles, cycles.length),
  } };
}

const data = boundedGraph({
  nodes: [{ id: 'app', label: 'app', version: '1.0', state: 'resolved', detail: 'root' }, { id: 'react', label: 'react', version: '18.3', state: 'conflict', detail: '18 and 19 requested' }, { id: 'scheduler', label: 'scheduler', version: '0.23', state: 'missing', detail: 'peer absent' }],
  edges: [{ source: 'app', target: 'react', relation: 'runtime', requirement: '^18' }, { source: 'react', target: 'scheduler', relation: 'peer', requirement: '^0.23' }, { source: 'scheduler', target: 'react', relation: 'runtime', requirement: '*' }],
  cycles: [['react', 'scheduler']], totals: { nodes: 87, edges: 240, cycles: 2 },
});

export function DependencyGraphStory() {
  const [issues, setIssues] = useState(false);
  const nodes = issues ? data.nodes.filter((node) => node.state !== 'resolved') : data.nodes;
  const ids = new Set(nodes.map((node) => node.id));
  return (
    <Column gap={2} grow={true}>
      <Heading label={'Dependency graph'} scale={'title'} />
      <Text label={'Inspect resolved packages, cross-dependencies, and verified cycles.'} />
      <Row gap={1}><Button label={issues ? 'Show all' : 'Show issues only'} onInvoke={() => setIssues(!issues)} /></Row>
      <DependencyGraph label={`${nodes.length} dependencies`} detail={`bounded source: nodes ${nodes.length}/${data.totals.nodes}, edges ${data.edges.length}/${data.totals.edges}, cycles ${data.cycles.length}/${data.totals.cycles}`}>
        {nodes.map((node) => <DependencyNode key={node.id} label={`${node.label}@${node.version}`} value={`id=${node.id} state=${node.state} detail=${node.detail}`}>
          {data.edges.filter((edge) => edge.source === node.id && ids.has(edge.target)).map((edge, index) => <DependencyEdge key={index} label={`${edge.relation} → ${edge.target}`} value={`requirement=${edge.requirement}`} />)}
        </DependencyNode>)}
        {data.cycles.map((cycle, index) => <DependencyCycle key={`c${index}`} label={`cycle ${index + 1}`} detail={`${cycle.length} members`}>
          {cycle.map((member, position) => <DependencyCycleMember key={position} label={member} value={`position=${position}`} />)}
        </DependencyCycle>)}
      </DependencyGraph>
      <InlineMessage label={'Explicit bounded-source totals remain visible after filtering.'} />
    </Column>
  );
}

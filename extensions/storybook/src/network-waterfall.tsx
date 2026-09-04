import React from 'react';
import { Button, Column, Heading, InlineMessage, NetworkPhase, NetworkRequest, NetworkWaterfall, Row, Text } from '@husklet/react';

const { useState } = React;
export const NETWORK_WATERFALL_STORY = 'Network waterfall';
export const REQUEST_LIMIT = 32;
export const PHASE_LIMIT = 6;
const METHODS = ['DELETE', 'GET', 'HEAD', 'OPTIONS', 'PATCH', 'POST', 'PUT'] as const;
const PHASES = ['dns', 'connect', 'tls', 'request', 'wait', 'download'] as const;
type Method = typeof METHODS[number];
type PhaseKind = typeof PHASES[number];
type NetworkPhaseRecord = { kind: PhaseKind; offsetUs: number; durationUs: number };
type NetworkRequestRecord = {
  method: Method;
  url: unknown;
  startUs: number;
  durationUs: number;
  status?: number | null;
  bytes: number;
  detail?: unknown;
  phases: readonly NetworkPhaseRecord[];
};
const clean = (value: unknown): string => String(value).replace(/[\u0000-\u001f\u007f-\u009f]/g, ' ').slice(0, 160);

function validPhase(value: unknown): value is NetworkPhaseRecord {
  if (value === null || typeof value !== 'object') return false;
  const phase = value as Record<string, unknown>;
  return typeof phase.kind === 'string' && (PHASES as readonly string[]).includes(phase.kind)
    && Number.isSafeInteger(phase.offsetUs) && Number(phase.offsetUs) >= 0
    && Number.isSafeInteger(phase.durationUs) && Number(phase.durationUs) > 0
    && Number(phase.durationUs) <= 3_600_000_000;
}

function validRequest(value: unknown): value is NetworkRequestRecord {
  if (value === null || typeof value !== 'object') return false;
  const request = value as Record<string, unknown>;
  if (typeof request.method !== 'string' || !(METHODS as readonly string[]).includes(request.method)
    || !clean(request.url).trim() || !Number.isSafeInteger(request.startUs) || Number(request.startUs) < 0
    || !Number.isSafeInteger(request.durationUs) || Number(request.durationUs) <= 0
    || Number(request.startUs) + Number(request.durationUs) > 86_400_000_000
    || !(request.status == null || Number.isSafeInteger(request.status) && Number(request.status) >= 100 && Number(request.status) <= 599)
    || !Number.isSafeInteger(request.bytes) || Number(request.bytes) < 0 || Number(request.bytes) > 2 ** 40
    || !Array.isArray(request.phases) || request.phases.length > PHASE_LIMIT || !request.phases.every(validPhase)) return false;
  const phases = request.phases;
  return phases.every((phase, index) => phase.offsetUs + phase.durationUs <= Number(request.durationUs)
    && (index === 0 || phases[index - 1].offsetUs + phases[index - 1].durationUs <= phase.offsetUs));
}

export function boundedRequests(requests: readonly unknown[], totalRequests: number = requests.length) {
  const valid = requests.filter(validRequest);
  const declaredTotal = Number.isSafeInteger(totalRequests) && totalRequests >= 0 ? totalRequests : valid.length;
  return { requests: valid.slice(0, REQUEST_LIMIT), total: Math.max(declaredTotal, valid.length) };
}

const fixture = boundedRequests([
  { method:'GET', url:'https://api.example.test/users?limit=20', startUs:0, durationUs:184000, status:200, bytes:18432, detail:'application/json', phases:[{kind:'dns',offsetUs:0,durationUs:12000},{kind:'connect',offsetUs:12000,durationUs:24000},{kind:'tls',offsetUs:36000,durationUs:31000},{kind:'request',offsetUs:67000,durationUs:4000},{kind:'wait',offsetUs:71000,durationUs:98000},{kind:'download',offsetUs:169000,durationUs:15000}] },
  { method:'POST', url:'https://api.example.test/session', startUs:205000, durationUs:96000, status:503, bytes:278, detail:'upstream unavailable', phases:[{kind:'connect',offsetUs:0,durationUs:18000},{kind:'request',offsetUs:18000,durationUs:9000},{kind:'wait',offsetUs:27000,durationUs:69000}] },
  { method:'GET', url:'https://cdn.example.test/avatar.png', startUs:318000, durationUs:75000, status:304, bytes:0, detail:'cache validated', phases:[{kind:'request',offsetUs:0,durationUs:3000},{kind:'wait',offsetUs:3000,durationUs:72000}] },
], 47);

export function NetworkWaterfallStory() {
  const [failedOnly, setFailedOnly] = useState(false);
  const shown = failedOnly ? fixture.requests.filter((request) => (request.status ?? 0) >= 400) : fixture.requests;
  return (
    <Column gap={2} grow={true}>
      <Heading label={'Network request waterfall'} scale={'title'} />
      <Text
        label={'Inspect exact request timing, transfer metadata, and ordered phases.'} />
      <Row gap={1}>
        <Button
          label={failedOnly?'Show all requests':'Show failures only'}
          onInvoke={()=>setFailedOnly(!failedOnly)} />
      </Row>
      <NetworkWaterfall
        label={`${shown.length} requests`}
        detail={`showing ${shown.length} of ${fixture.total} requests`}
        gap={1}>
        {shown.map((request,index)=><NetworkRequest
          key={index}
          label={`${request.method} ${clean(request.url)}`}
          value={`start_us=${request.startUs} duration_us=${request.durationUs} status=${request.status ?? 'pending'} bytes=${request.bytes} detail=${clean(request.detail)}`}>
          {request.phases.map((phase,i)=><NetworkPhase
            key={i}
            label={phase.kind}
            value={`offset_us=${phase.offsetUs} duration_us=${phase.durationUs} total_us=${request.durationUs}`} />)}
        </NetworkRequest>)}
      </NetworkWaterfall>
      <InlineMessage
        label={`Bounded source: ${fixture.total - shown.length} requests are outside this projection.`} />
    </Column>
  );
}

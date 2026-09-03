import React from 'react';
import { Button, Column, Heading, InlineMessage, NetworkPhase, NetworkRequest, NetworkWaterfall, Row, Text } from '@husklet/react';

const { createElement: h, useState } = React;
export const NETWORK_WATERFALL_STORY = 'Network waterfall';
export const REQUEST_LIMIT = 32;
export const PHASE_LIMIT = 6;
const clean = (value) => String(value).replace(/[\u0000-\u001f\u007f-\u009f]/g, ' ').slice(0, 160);

export function boundedRequests(requests, totalRequests = requests.length) {
  const valid = requests.filter((r) => r && ['DELETE','GET','HEAD','OPTIONS','PATCH','POST','PUT'].includes(r.method)
    && clean(r.url).trim() && Number.isInteger(r.startUs) && Number.isInteger(r.durationUs) && r.durationUs > 0
    && r.startUs >= 0 && r.startUs + r.durationUs <= 86400000000 && (r.status == null || Number.isInteger(r.status) && r.status >= 100 && r.status <= 599)
    && Number.isInteger(r.bytes) && r.bytes >= 0 && r.bytes <= 2 ** 40
    && r.phases.length <= PHASE_LIMIT && r.phases.every((p, i, phases) => ['dns','connect','tls','request','wait','download'].includes(p.kind) && p.durationUs > 0 && p.durationUs <= 3600000000 && p.offsetUs + p.durationUs <= r.durationUs && (i === 0 || phases[i - 1].offsetUs + phases[i - 1].durationUs <= p.offsetUs)));
  return { requests: valid.slice(0, REQUEST_LIMIT), total: Math.max(totalRequests, valid.length) };
}

const fixture = boundedRequests([
  { method:'GET', url:'https://api.example.test/users?limit=20', startUs:0, durationUs:184000, status:200, bytes:18432, detail:'application/json', phases:[{kind:'dns',offsetUs:0,durationUs:12000},{kind:'connect',offsetUs:12000,durationUs:24000},{kind:'tls',offsetUs:36000,durationUs:31000},{kind:'request',offsetUs:67000,durationUs:4000},{kind:'wait',offsetUs:71000,durationUs:98000},{kind:'download',offsetUs:169000,durationUs:15000}] },
  { method:'POST', url:'https://api.example.test/session', startUs:205000, durationUs:96000, status:503, bytes:278, detail:'upstream unavailable', phases:[{kind:'connect',offsetUs:0,durationUs:18000},{kind:'request',offsetUs:18000,durationUs:9000},{kind:'wait',offsetUs:27000,durationUs:69000}] },
  { method:'GET', url:'https://cdn.example.test/avatar.png', startUs:318000, durationUs:75000, status:304, bytes:0, detail:'cache validated', phases:[{kind:'request',offsetUs:0,durationUs:3000},{kind:'wait',offsetUs:3000,durationUs:72000}] },
], 47);

export function NetworkWaterfallStory() {
  const [failedOnly, setFailedOnly] = useState(false);
  const shown = failedOnly ? fixture.requests.filter((request) => (request.status ?? 0) >= 400) : fixture.requests;
  return h(Column, { gap:2, grow:true }, h(Heading,{label:'Network request waterfall',scale:'title'}), h(Text,{label:'Inspect exact request timing, transfer metadata, and ordered phases.'}),
    h(Row,{gap:1},h(Button,{label:failedOnly?'Show all requests':'Show failures only',onInvoke:()=>setFailedOnly(!failedOnly)})),
    h(NetworkWaterfall,{label:`${shown.length} requests`,detail:`showing ${shown.length} of ${fixture.total} requests`,gap:1}, ...shown.map((request,index)=>h(NetworkRequest,{key:index,label:`${request.method} ${clean(request.url)}`,value:`start_us=${request.startUs} duration_us=${request.durationUs} status=${request.status ?? 'pending'} bytes=${request.bytes} detail=${clean(request.detail)}`}, ...request.phases.map((phase,i)=>h(NetworkPhase,{key:i,label:phase.kind,value:`offset_us=${phase.offsetUs} duration_us=${phase.durationUs} total_us=${request.durationUs}`}))))),
    h(InlineMessage,{label:`Bounded source: ${fixture.total - shown.length} requests are outside this projection.`}));
}

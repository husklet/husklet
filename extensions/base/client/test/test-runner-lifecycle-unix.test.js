import assert from 'node:assert/strict';
import net from 'node:net';
import os from 'node:os';
import path from 'node:path';
import { mkdtemp, rm } from 'node:fs/promises';
import test from 'node:test';
import {
  ExecutionOperationError,
  ExecutionOutputEndedEarlyError,
  connect,
  workspace,
} from '../dist/index.js';
import { CONTROL, KIND, Reader, encode } from '../dist/wire.js';

test('test runner refuses output EOF from a live execution, cancels it, and reuses the session', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-test-runner-eof-'));
  const socketPath = path.join(directory, 'host.sock');
  const containerId = 'c'.repeat(64);
  const executionId = 'e'.repeat(32);
  const calls = [];
  const connections = new Set();
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.kind !== KIND.request) continue;
        calls.push(frame.payload.call);
        const payload =
          frame.payload.call === 'container_exec'
            ? { reply: 'identity', with: executionId }
            : frame.payload.call === 'execution_output'
              ? {
                  reply: 'execution_output',
                  with: { entries: [], next: 0, more: false, eof: true, gap: false },
                }
              : frame.payload.call === 'execution_inspect'
                ? {
                    reply: 'execution',
                    with: {
                      id: executionId,
                      container_id: containerId,
                      running: true,
                      exit_code: -1,
                      pid: 42,
                      command: ['npm', 'test', '--', '--reporter=jsonl'],
                      user: 'runner',
                    },
                  }
                : frame.payload.call === 'execution_cancel'
                  ? { reply: 'done' }
                  : {
                      reply: 'workspace',
                      with: { name: 'tests', image: 'toolbox', architecture: 'amd64' },
                    };
        const response = encode({ channel: frame.channel, kind: KIND.response, payload });
        for (const byte of response) socket.write(Buffer.of(byte));
      }
    });
    const greeting = encode({
      channel: CONTROL,
      kind: KIND.open,
      payload: {
        protocol: 1,
        peer: 'test-runner',
        granted: ['containers:execute', 'containers:read', 'workspaces:read'],
      },
    });
    socket.write(greeting.subarray(0, 3));
    socket.write(greeting.subarray(3));
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));

  try {
    const session = await connect({ path: socketPath });
    const host = workspace(session);
    let pages = 0;
    await assert.rejects(
      host.containers.execStreaming(
        containerId,
        9,
        { command: ['npm', 'test', '--', '--reporter=jsonl'], pageLimit: 1 },
        () => {
          pages += 1;
        },
      ),
      (error) => {
        assert(error instanceof ExecutionOperationError);
        assert.equal(error.phase, 'inspect');
        assert(
          error.cause instanceof ExecutionOutputEndedEarlyError,
          `unexpected inspect cause: ${error.cause?.stack ?? error.cause}`,
        );
        assert.equal(error.cause.executionId, executionId);
        return true;
      },
    );
    assert.equal(pages, 1, 'the bounded EOF page was delivered exactly once');
    assert.deepEqual(calls.slice(0, 4), [
      'container_exec',
      'execution_output',
      'execution_inspect',
      'execution_cancel',
    ]);
    assert.equal((await host.info()).name, 'tests', 'typed refusal keeps ordered session reusable');
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

test('test runner failure preserves only its acknowledged output cursor over fragmented Unix framing', async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), 'husklet-test-runner-cursor-'));
  const socketPath = path.join(directory, 'host.sock');
  const containerId = 'c'.repeat(64);
  const executionId = 'e'.repeat(32);
  const calls = [];
  const connections = new Set();
  const server = net.createServer((socket) => {
    connections.add(socket);
    socket.on('close', () => connections.delete(socket));
    const reader = new Reader();
    socket.on('data', (chunk) => {
      for (const frame of reader.take(chunk)) {
        if (frame.kind !== KIND.request) continue;
        calls.push(frame.payload);
        const after = frame.payload.with?.after;
        const payload =
          frame.payload.call === 'container_exec'
            ? { reply: 'identity', with: executionId }
            : frame.payload.call === 'execution_output'
              ? {
                  reply: 'execution_output',
                  with: {
                    entries: [
                      {
                        sequence: after + 1,
                        timestamp_ms: after + 1,
                        stream: 'stdout',
                        bytes: [...Buffer.from(`{"case":${after + 1}}\n`)],
                      },
                    ],
                    next: after + 1,
                    more: after === 0,
                    eof: after !== 0,
                    gap: false,
                  },
                }
              : frame.payload.call === 'execution_cancel'
                ? { reply: 'done' }
                : {
                    reply: 'workspace',
                    with: { name: 'tests', image: 'toolbox', architecture: 'amd64' },
                  };
        const response = encode({ channel: frame.channel, kind: KIND.response, payload });
        for (const byte of response) socket.write(Buffer.of(byte));
      }
    });
    const greeting = encode({
      channel: CONTROL,
      kind: KIND.open,
      payload: {
        protocol: 1,
        peer: 'test-runner-cursor',
        granted: ['containers:execute', 'containers:read', 'workspaces:read'],
      },
    });
    socket.write(greeting.subarray(0, 3));
    socket.write(greeting.subarray(3));
  });
  await new Promise((resolve) => server.listen(socketPath, resolve));

  try {
    const session = await connect({ path: socketPath });
    const host = workspace(session);
    const committed = [];
    await assert.rejects(
      host.containers.execStreaming(
        containerId,
        4,
        { command: ['tests', '--jsonl'], pageLimit: 1, pollIntervalMs: 10 },
        async (page) => {
          if (page.next === 2) throw new Error('result database unavailable');
          committed.push(page.next);
        },
      ),
      (error) => {
        assert(error instanceof ExecutionOperationError);
        assert.equal(error.executionId, executionId);
        assert.equal(error.phase, 'output');
        assert.equal(error.after, 1);
        return true;
      },
    );
    assert.deepEqual(committed, [1]);
    assert.deepEqual(
      calls.filter(({ call }) => call === 'execution_output').map(({ with: value }) => value.after),
      [0, 1],
    );
    assert.equal(calls.filter(({ call }) => call === 'execution_cancel').length, 1);
    assert.equal((await host.info()).name, 'tests');
    await session.close();
  } finally {
    for (const connection of connections) connection.destroy();
    await new Promise((resolve) => server.close(resolve));
    await rm(directory, { recursive: true, force: true });
  }
});

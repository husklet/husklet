// The conversation with the host: connect, greet, call, and answer.

import net from 'node:net';
import { CONTROL, KIND, Reader, encode } from './wire.js';
import {
  encodeRequest, PROTOCOL_TOPICS, PROTOCOL_VERSION,
  validateFailure, validateReplyFor, validateSnapshot,
} from './generated-protocol.js';

/** The protocol this package speaks. The host refuses anything else. */
export const PROTOCOL = PROTOCOL_VERSION;

/** Where the host mounts the socket inside an extension's container. */
export const SOCKET = 'HUSKLET_EXTENSION_SOCKET';

/** The protocol's shared call channel. Replies are ordered on this channel. */
const CALLS = 2;
const ERROR = 2;
const SNAPSHOT_TOPICS = new Map(PROTOCOL_TOPICS.map(({ wire, snapshot }) => [snapshot, wire]));

function requiredObject(value, label) {
  if (!value || typeof value !== 'object' || Array.isArray(value)) throw new TypeError(`${label} must be an object`);
  return value;
}

/** GUI interaction frames are not protocol Snapshots and retain their own wire vocabulary. */
export function validateUiEvent(value) {
  const event = requiredObject(value, 'UI event');
  if (typeof event.pane_provider === 'string' && typeof event.slot === 'string') return value;
  if (typeof event.interaction === 'string') {
    if (!['invoke', 'submit', 'change', 'select', 'scroll', 'close', 'context', 'key', 'focus', 'pointer', 'drag', 'drop'].includes(event.interaction)
      || typeof event.trigger !== 'string' || !Number.isSafeInteger(event.node) || event.node < 0
      || typeof event.id !== 'string' || event.id.length === 0 || event.id.length > 4096
      || (event.slot !== undefined && typeof event.slot !== 'string')) throw new TypeError('UI interaction event is malformed');
    return value;
  }
  // Protocol-1 hosts used either a string event tag or an externally-tagged
  // event object. Keep that compatibility path typed and bounded.
  if (typeof event.event === 'string') {
    if (!Number.isSafeInteger(event.node) || event.node < 0 || typeof event.id !== 'string' || event.id.length > 4096) throw new TypeError('legacy UI event is malformed');
    return value;
  }
  if (event.event && typeof event.event === 'object' && !Array.isArray(event.event) && Object.keys(event.event).length === 1) {
    const body = requiredObject(Object.values(event.event)[0], 'legacy UI event body');
    if (!Number.isSafeInteger(body.node) || body.node < 0 || typeof body.id !== 'string' || body.id.length > 4096) throw new TypeError('legacy UI event is malformed');
    return value;
  }
  throw new TypeError('event is neither a protocol snapshot nor a known UI event');
}

/** Refusal returned by the host, with its stable machine-readable category. */
export class ExtensionError extends Error {
  constructor(failure) {
    const kind = failure?.error ?? 'failed';
    super(failure?.detail ?? failure?.call ?? `extension call ${kind}`);
    this.name = 'ExtensionError';
    this.kind = kind;
    this.capability = failure?.capability;
  }
}

/**
 * One connected extension.
 *
 * The host answers calls in order on one channel. A bounded FIFO correlates
 * those answers with promises without inventing request identifiers the wire
 * protocol does not carry.
 */
export class Session {
  #socket;
  #reader = new Reader();
  #onReply;
  #onRows;
  #onEventError;
  #onClose;
  #events = new Set();
  #topics = new Set();
  #eventTopics = new Map();
  #pending = [];
  #limit;
  #timeout;
  #closed = false;
  #granted = [];
  #greeted;
  #ready;
  #rejectReady;

  constructor(socket, {
    onReply = () => {}, onRows = () => {}, onEvent = () => {}, onEventError = () => {}, onClose = () => {}, pendingLimit = 64, timeout = 30_000,
  } = {}) {
    if (!Number.isSafeInteger(pendingLimit) || pendingLimit < 1) throw new RangeError('pendingLimit must be a positive integer');
    if (!Number.isFinite(timeout) || timeout <= 0) throw new RangeError('timeout must be positive');
    this.#socket = socket;
    this.#limit = pendingLimit;
    this.#timeout = timeout;
    // Resolved once the host has greeted us and we have answered. Nothing may
    // be sent before that: the host reads the first frame it receives as the
    // greeting, so a call written earlier is swallowed and every call after it
    // arrives one step out of order.
    this.#greeted = new Promise((resolve, reject) => {
      this.#ready = resolve;
      this.#rejectReady = reject;
    });
    this.#onReply = onReply;
    this.#onRows = onRows;
    this.#onEventError = onEventError;
    this.#onClose = onClose;
    this.#events.add(onEvent);
    socket.on('data', (chunk) => this.#receive(chunk));
    socket.on('close', () => this.#finish(new Error('extension host connection closed')));
    socket.on('error', (error) => this.#finish(error));
  }

  /** Capabilities the host granted, known once the greeting arrives. */
  get granted() {
    return this.#granted;
  }

  /** Resolves when the handshake is complete and calls may be sent. */
  get ready() {
    return this.#greeted;
  }

  /** Opens the socket the host provided. */
  static connect(path = process.env[SOCKET], handlers = {}) {
    if (!path) throw new Error(`${SOCKET} is not set; an extension runs inside a workspace`);
    const connectTimeout = handlers.connectTimeout ?? 30_000;
    if (!Number.isFinite(connectTimeout) || connectTimeout <= 0) throw new RangeError('connectTimeout must be positive');
    return new Promise((resolve, reject) => {
      const socket = net.createConnection(path);
      let settled = false;
      const timer = setTimeout(() => {
        if (settled) return;
        settled = true;
        socket.destroy();
        reject(new Error(`extension host handshake timed out after ${connectTimeout}ms`));
      }, connectTimeout);
      const fail = (error) => {
        if (settled) return;
        settled = true;
        clearTimeout(timer);
        socket.destroy();
        reject(error);
      };
      socket.once('error', fail);
      // Resolve only after the handshake, so a caller that renders straight
      // away cannot outrun it.
      socket.once('connect', () => {
        const session = new Session(socket, handlers);
        session.ready.then(() => {
          if (settled) return session.close();
          settled = true;
          clearTimeout(timer);
          socket.removeListener('error', fail);
          resolve(session);
        }).catch(fail);
      });
    });
  }

  /** Sends one call and resolves with the tagged host reply. */
  call(name, argument) {
    if (this.#closed) return Promise.reject(new Error('extension session is closed'));
    if (this.#pending.length >= this.#limit) {
      return Promise.reject(new Error(`extension call limit of ${this.#limit} is exhausted`));
    }
    const payload = encodeRequest(name, argument);
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        const error = new Error(`extension call ${name} timed out after ${this.#timeout}ms`);
        // Without request identifiers, continuing after one missing ordered
        // reply could give every later caller the wrong answer. End the session.
        this.#finish(error);
        this.#socket.destroy();
      }, this.#timeout);
      this.#pending.push({ resolve, reject, timer, name, argument });
      try {
        this.#socket.write(encode({ channel: CALLS, kind: KIND.request, payload }));
      } catch (error) {
        clearTimeout(timer);
        this.#pending.pop();
        reject(error);
      }
    });
  }

  /** Answers a row window the host asked for. */
  answer(channel, window) {
    this.#socket.write(encode({ channel, kind: KIND.response, payload: window }));
  }

  /** Adds a pushed-event observer and returns a synchronous disposer. */
  onEvent(listener) {
    if (typeof listener !== 'function') throw new TypeError('event listener must be a function');
    this.#events.add(listener);
    return () => this.#events.delete(listener);
  }

  close() {
    this.#finish(new Error('extension session closed'));
    this.#socket.end();
  }

  #receive(chunk) {
    try {
      for (const frame of this.#reader.take(chunk)) this.#handle(frame);
    } catch (error) {
      this.#finish(error);
      this.#socket.destroy();
    }
  }

  #handle(frame) {
    if (frame.channel === CONTROL) return this.#greet(frame);
    // A row request is the one thing the host pushes rather than answers.
    if (frame.payload && frame.payload.range !== undefined) {
      return this.#onRows(frame.payload, frame.channel);
    }
    if (frame.kind === KIND.response && frame.channel === CALLS) {
      const pending = this.#pending[0];
      if (!pending) return this.#onReply(frame.payload);
      const payload = (frame.flags & ERROR) !== 0
        ? validateFailure(frame.payload)
        : validateReplyFor(pending.name, frame.payload);
      this.#pending.shift();
      clearTimeout(pending.timer);
      if ((frame.flags & ERROR) !== 0) pending.reject(new ExtensionError(payload));
      else {
        if (pending.name === 'event_subscribe') this.#topics.add(pending.argument.topic);
        if (pending.name === 'event_unsubscribe') {
          this.#topics.delete(pending.argument.topic);
          for (const [channel, topic] of this.#eventTopics) if (topic === pending.argument.topic) this.#eventTopics.delete(channel);
        }
        pending.resolve(payload);
      }
      this.#onReply(payload);
      return;
    }
    if (frame.kind === KIND.event) {
      let payload = frame.payload;
      if (typeof payload?.snapshot === 'string') {
        payload = validateSnapshot(payload);
        const topic = SNAPSHOT_TOPICS.get(payload.snapshot);
        if (!topic || !this.#topics.has(topic)) throw new TypeError(`snapshot ${payload.snapshot} has no active subscription`);
        const bound = this.#eventTopics.get(frame.channel);
        if (bound !== undefined && bound !== topic) throw new TypeError(`event channel ${frame.channel} changed topic`);
        this.#eventTopics.set(frame.channel, topic);
      } else {
        payload = validateUiEvent(payload);
      }
      try {
        for (const listener of this.#events) {
          try {
            listener(payload, frame.channel);
          } catch (error) {
            try { this.#onEventError(error); } catch { /* Error reporting cannot strand event credit. */ }
          }
        }
      } finally {
        // Returning one credit after attempting every listener bounds a producer
        // without allowing one faulty observer to stall the whole event stream.
        this.#socket.write(encode({ channel: frame.channel, kind: KIND.credit, payload: 1 }));
      }
      return;
    }
    return this.#onReply(frame.payload);
  }

  #finish(error) {
    if (this.#closed) return;
    this.#closed = true;
    this.#rejectReady(error);
    for (const pending of this.#pending.splice(0)) {
      clearTimeout(pending.timer);
      pending.reject(error);
    }
    try { this.#onClose(error); } catch { /* Lifecycle reporting cannot prevent closure. */ }
  }

  /** The host speaks first and states the grant, so an extension knows what it
   * holds before it asks for anything. */
  #greet(frame) {
    const welcome = frame.payload;
    if (!welcome || welcome.protocol === undefined) return;
    if (welcome.protocol !== PROTOCOL) {
      throw new Error(`host speaks protocol ${welcome.protocol}, this extension speaks ${PROTOCOL}`);
    }
    this.#granted = welcome.granted ?? [];
    this.#socket.write(
      encode({
        channel: CONTROL,
        kind: KIND.response,
        payload: { protocol: PROTOCOL, name: welcome.peer ?? welcome.extension, features: [] },
      }),
    );
    this.#ready();
  }
}

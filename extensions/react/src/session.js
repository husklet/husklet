// The conversation with the host: connect, greet, call, and answer.

import net from 'node:net';
import { CONTROL, KIND, Reader, encode } from './wire.js';

/** The protocol this package speaks. The host refuses anything else. */
export const PROTOCOL = 1;

/** Where the host mounts the socket inside an extension's container. */
export const SOCKET = 'HUSKLET_EXTENSION_SOCKET';

/** The channel calls travel on. Odd, because the host allocates the even ones. */
const CALLS = 1;

/**
 * One connected extension.
 *
 * Calls are fire and forget: the host answers in order, and nothing here needs
 * to correlate a reply with its question. Replies and pushed events reach the
 * handlers given at construction.
 */
export class Session {
  #socket;
  #reader = new Reader();
  #onReply;
  #onRows;
  #granted = [];
  #greeted;
  #ready;

  constructor(socket, { onReply = () => {}, onRows = () => {} } = {}) {
    this.#socket = socket;
    // Resolved once the host has greeted us and we have answered. Nothing may
    // be sent before that: the host reads the first frame it receives as the
    // greeting, so a call written earlier is swallowed and every call after it
    // arrives one step out of order.
    this.#greeted = new Promise((resolve) => {
      this.#ready = resolve;
    });
    this.#onReply = onReply;
    this.#onRows = onRows;
    socket.on('data', (chunk) => this.#receive(chunk));
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
  static connect(path = process.env[SOCKET], handlers) {
    if (!path) throw new Error(`${SOCKET} is not set; an extension runs inside a workspace`);
    return new Promise((resolve, reject) => {
      const socket = net.createConnection(path);
      socket.once('error', reject);
      // Resolve only after the handshake, so a caller that renders straight
      // away cannot outrun it.
      socket.once('connect', () => {
        const session = new Session(socket, handlers);
        session.ready.then(() => resolve(session)).catch(reject);
      });
    });
  }

  /** Sends one call. */
  call(name, argument) {
    const payload = argument === undefined ? { call: name } : { call: name, with: argument };
    this.#socket.write(encode({ channel: CALLS, kind: KIND.request, payload }));
  }

  /** Answers a row window the host asked for. */
  answer(channel, window) {
    this.#socket.write(encode({ channel, kind: KIND.response, payload: window }));
  }

  close() {
    this.#socket.end();
  }

  #receive(chunk) {
    for (const frame of this.#reader.take(chunk)) {
      this.#handle(frame);
    }
  }

  #handle(frame) {
    if (frame.channel === CONTROL) return this.#greet(frame);
    // A row request is the one thing the host pushes rather than answers.
    if (frame.payload && frame.payload.range !== undefined) {
      return this.#onRows(frame.payload, frame.channel);
    }
    return this.#onReply(frame.payload);
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

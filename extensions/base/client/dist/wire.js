// Framing, byte for byte as the host writes it.
//
// A fixed twelve byte header, little endian: payload length, channel, kind,
// flags, and two reserved bytes that must be zero. The host refuses anything
// else, so this file is the one place the layout is written down on this side.
import { Buffer } from 'node:buffer';
export const KIND = {
    request: 1,
    response: 2,
    event: 3,
    open: 4,
    close: 5,
    reset: 6,
    credit: 7,
    ping: 8,
    pong: 9,
};
export const FLAG_END = 1;
export const CONTROL = 0;
export const HEADER = 12;
/// Largest payload the host accepts. Anything above is refused before it is read.
// A protocol payload may contain a bounded 1 MiB byte array. Its JSON wire
// representation needs up to four bytes per value, plus the call envelope.
export const PAYLOAD_LIMIT = 5 << 20;
const KNOWN_FLAGS = 0b0000_0111;
const KINDS = new Set(Object.values(KIND));
const UTF8 = new TextDecoder('utf-8', { fatal: true });
const CAPACITY = HEADER + PAYLOAD_LIMIT;
const JSON_DEPTH_LIMIT = 128;
function validateJson(value) {
    const pending = [[value, 0]];
    while (pending.length > 0) {
        const [current, depth] = pending.pop();
        if (typeof current === 'number' &&
            (!Number.isFinite(current) || (Number.isInteger(current) && !Number.isSafeInteger(current)))) {
            throw new Error('frame JSON contains a number that cannot cross the protocol losslessly');
        }
        if (!current || typeof current !== 'object')
            continue;
        if (depth >= JSON_DEPTH_LIMIT)
            throw new Error(`frame JSON exceeds the ${JSON_DEPTH_LIMIT} level depth limit`);
        for (const child of Array.isArray(current) ? current : Object.values(current))
            pending.push([child, depth + 1]);
    }
    return value;
}
/** Encodes one frame. */
export function encode({ channel = CONTROL, kind, payload, flags = FLAG_END }) {
    if (!Number.isInteger(channel) || channel < 0 || channel > 0xffff_ffff)
        throw new RangeError('frame channel must be an unsigned 32-bit integer');
    if (!KINDS.has(kind))
        throw new RangeError(`frame has unknown kind ${kind}`);
    if (!Number.isInteger(flags) || flags < 0 || flags > 0xff || (flags & ~KNOWN_FLAGS) !== 0)
        throw new RangeError(`frame has unknown flags ${flags}`);
    const serialized = payload instanceof Uint8Array ? undefined : JSON.stringify(payload);
    if (serialized === undefined && !(payload instanceof Uint8Array))
        throw new TypeError('frame payload must be bytes or a JSON value');
    const body = Buffer.isBuffer(payload) || payload instanceof Uint8Array
        ? Buffer.from(payload)
        : Buffer.from(serialized, 'utf8');
    if (body.length > PAYLOAD_LIMIT) {
        throw new Error(`frame payload is ${body.length} bytes, above the ${PAYLOAD_LIMIT} limit`);
    }
    const frame = Buffer.allocUnsafe(HEADER + body.length);
    frame.writeUInt32LE(body.length, 0);
    frame.writeUInt32LE(channel, 4);
    frame.writeUInt8(kind, 8);
    frame.writeUInt8(flags, 9);
    frame.writeUInt16LE(0, 10);
    body.copy(frame, HEADER);
    return frame;
}
/**
 * Accumulates bytes and yields whole frames.
 *
 * A socket hands over arbitrary slices, so a frame arrives in pieces and
 * several arrive together; both are ordinary and neither is an error.
 */
export class Reader {
    #held = Buffer.allocUnsafe(CAPACITY);
    #length = 0;
    /** Bytes retained for the incomplete frame at the front of the stream. */
    get buffered() {
        return this.#length;
    }
    /** Adds bytes and returns every frame they completed. */
    take(chunk) {
        if (!(chunk instanceof Uint8Array))
            throw new TypeError('frame chunk must be bytes');
        const frames = [];
        let offset = 0;
        while (offset < chunk.length) {
            const room = CAPACITY - this.#length;
            if (room === 0)
                throw new Error(`frame exceeds the ${PAYLOAD_LIMIT} byte payload limit`);
            const part = chunk.subarray(offset, offset + room);
            Buffer.from(part.buffer, part.byteOffset, part.byteLength).copy(this.#held, this.#length);
            this.#length += part.length;
            offset += part.length;
            for (;;) {
                const frame = this.#next();
                if (frame === null)
                    break;
                frames.push(frame);
            }
        }
        return frames;
    }
    #next() {
        if (this.#length < HEADER)
            return null;
        const length = this.#held.readUInt32LE(0);
        if (length > PAYLOAD_LIMIT) {
            throw new Error(`frame declares ${length} bytes, above the ${PAYLOAD_LIMIT} limit`);
        }
        const kind = this.#held.readUInt8(8);
        const flags = this.#held.readUInt8(9);
        if (!KINDS.has(kind))
            throw new Error(`frame has unknown kind ${kind}`);
        if ((flags & ~KNOWN_FLAGS) !== 0)
            throw new Error(`frame has unknown flags ${flags}`);
        if (this.#held.readUInt16LE(10) !== 0)
            throw new Error('frame reserved bytes must be zero');
        const total = HEADER + length;
        if (this.#length < total)
            return null;
        const body = this.#held.subarray(HEADER, total);
        let payload = Buffer.from(body);
        if (![KIND.ping, KIND.pong, KIND.close, KIND.reset].includes(kind)) {
            try {
                payload = validateJson(JSON.parse(UTF8.decode(body)));
            }
            catch (error) {
                throw new Error(`frame payload is not valid UTF-8 JSON: ${error.message}`, {
                    cause: error,
                });
            }
        }
        const frame = {
            channel: this.#held.readUInt32LE(4),
            kind,
            flags,
            payload,
        };
        this.#held.copyWithin(0, total, this.#length);
        this.#length -= total;
        return frame;
    }
    /** Refuses an EOF that cut a header or payload short. */
    finish() {
        if (this.#length !== 0) {
            const held = this.#length;
            this.#length = 0;
            throw new Error(`extension host closed with an unfinished frame (${held} bytes buffered)`);
        }
    }
}

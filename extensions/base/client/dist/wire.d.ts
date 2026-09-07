import { Buffer } from 'node:buffer';
export declare const KIND: {
    request: number;
    response: number;
    event: number;
    open: number;
    close: number;
    reset: number;
    credit: number;
    ping: number;
    pong: number;
};
export declare const FLAG_END = 1;
export declare const CONTROL = 0;
export declare const HEADER = 12;
export declare const PAYLOAD_LIMIT: number;
/** Encodes one frame. */
export declare function encode({ channel, kind, payload, flags }: {
    channel?: number;
    kind: any;
    payload: any;
    flags?: number;
}): Buffer<ArrayBuffer>;
/**
 * Accumulates bytes and yields whole frames.
 *
 * A socket hands over arbitrary slices, so a frame arrives in pieces and
 * several arrive together; both are ordinary and neither is an error.
 */
export declare class Reader {
    #private;
    /** Bytes retained for the incomplete frame at the front of the stream. */
    get buffered(): number;
    /** Adds bytes and returns every frame they completed. */
    take(chunk: any): any[];
    /** Refuses an EOF that cut a header or payload short. */
    finish(): void;
}

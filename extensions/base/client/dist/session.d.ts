import type { CallOptions, ConnectOptions, HostEvent, RowRequest, ReadonlyFilesystemGrant } from './api.js';
/** The protocol this package speaks. The host refuses anything else. */
export declare const PROTOCOL: 1;
/** Where the host mounts the socket inside an extension's container. */
export declare const SOCKET = "HUSKLET_EXTENSION_SOCKET";
/** Environment variable naming this extension's private durable data directory. */
export declare const DATA = "HUSKLET_EXTENSION_DATA";
/** Validate the host-pushed request before extension code uses it for database paging. */
export declare function validateRowRequest(value: any): RowRequest;
/** GUI interaction frames are not protocol Snapshots and retain their own wire vocabulary. */
export declare function validateUiEvent(value: any): any;
/** Refusal returned by the host, with its stable machine-readable category. */
export declare class ExtensionError extends Error {
    readonly kind: any;
    readonly capability: any;
    constructor(failure: any);
}
/** A row answer does not belong to the outstanding host request on its channel. */
export declare class RowReplyMismatchError extends Error {
    readonly channel: any;
    readonly request: any;
    constructor(channel: any, request: any);
}
/** A row channel has no unanswered request and cannot accept a late or duplicate answer. */
export declare class RowRequestUnavailableError extends Error {
    readonly channel: any;
    constructor(channel: any);
}
/**
 * One connected extension.
 *
 * The host answers calls in order on one channel. A bounded FIFO correlates
 * those answers with promises without inventing request identifiers the wire
 * protocol does not carry.
 */
export declare class Session {
    #private;
    constructor(socket: any, { onReply, onRows, onEvent, onEventError, onClose, pendingLimit, timeout, }?: ConnectOptions);
    /** Capabilities the host granted, known once the greeting arrives. */
    get granted(): readonly string[];
    /** Immutable exact wire capabilities negotiated with the host. */
    get grantedCapabilities(): readonly string[];
    /** Immutable exact filesystem selectors granted to this connected extension. */
    get grantedFilesystem(): ReadonlyFilesystemGrant;
    /** Resolves when the handshake is complete and calls may be sent. */
    get ready(): any;
    /** Resolves once with the reason this session ended. */
    get closed(): any;
    /** Opens the socket the host provided. */
    static connect(path?: any, handlers?: ConnectOptions): Promise<unknown>;
    /** Sends one call and resolves with the tagged host reply. */
    call(name: any, argument?: unknown, { signal }?: CallOptions): Promise<unknown>;
    /** Answers a row window the host asked for. */
    answer(channel: any, window: any): void;
    /** Round-trips an opaque bounded heartbeat without consuming call ordering. */
    ping(): Promise<unknown>;
    /** Adds a pushed-event observer and returns a synchronous disposer. */
    onEvent(listener: (event: HostEvent, channel: number) => void | Promise<void>): () => void;
    close(): any;
}

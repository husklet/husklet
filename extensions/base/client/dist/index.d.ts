export { ExtensionError, Session, SOCKET, PROTOCOL, validateRowRequest, validateUiEvent, } from './session.js';
export { PROTOCOL_SPECIFICATION_VERSION, PROTOCOL_VERSION, PROTOCOL_BOUNDS, PROTOCOL_CAPABILITIES, PROTOCOL_TOPICS, PROTOCOL_REPLIES, PROTOCOL_REQUEST_CAPABILITIES, encodeRequest, validateRequest, validateReply, validateReplyFor, validateFailure, validateSnapshot, } from './generated-protocol.js';
import { semanticXml } from './semantic.js';
export { semanticXml };
import type { CallOptions, ConnectOptions, Session as ClientSession, WorkspaceApi } from './api.js';
/** A post-creation execution failure whose immutable identity remains recoverable. */
export declare class ExecutionOperationError extends Error {
    readonly executionId: any;
    readonly phase: any;
    readonly execution: any;
    constructor(executionId: any, phase: any, cause: any, execution?: any);
}
/** Output retention advanced past the cursor, so a transcript/result would be incomplete. */
export declare class ExecutionOutputGapError extends Error {
    readonly executionId: any;
    readonly after: any;
    readonly next: any;
    constructor(executionId: any, after: any, next: any);
}
/** A terminal authority succeeded, but its bounded observation could not be completed. */
export declare class TerminalOperationError extends Error {
    readonly operation: any;
    readonly result: any;
    constructor(operation: any, result: any, cause: any);
}
export declare function connect(options?: ConnectOptions): Promise<unknown>;
/**
 * Opens a surface and paints a dependency-free first frame.
 *
 * Extensions can do this before importing React or another renderer, keeping
 * cold-start feedback independent of framework initialization. Pass the
 * returned token to the renderer so it continues the same frame sequence.
 */
export declare function bootstrapSurface(session: any, { title, label, primary }?: {
    title?: string;
    label?: string;
    primary?: boolean;
}): Promise<Readonly<{
    slot: string;
    sequence: 1;
    nextNode: 2;
    bootstrapNode: 1;
}>>;
export declare function workspace(session: ClientSession, { signal }?: CallOptions): WorkspaceApi;
/** Mirrors Rust Request::capability for every fixed wire call used by this public facade. */
export declare function requestCapability(call: any): any;
/** Schema-derived inventory connecting every Rust request/topic to its supported public route. */
export declare const protocolSurface: Readonly<{
    requests: any;
    topics: Readonly<{
        [k: string]: Readonly<{
            subscribe: "subscribe";
            unsubscribe: "unsubscribe";
        }>;
    }>;
}>;
/** Honest inventory of the current host contract; gaps are not callable APIs. */
export declare const protocolCoverage: Readonly<{
    available: Readonly<{
        workspace: string[];
        containers: string[];
        images: string[];
        volumes: string[];
        networks: string[];
        terminal: string[];
        files: string[];
        state: string[];
        extensions: string[];
        notifications: string[];
        interfaceEvents: string[];
        workspaceEvents: string[];
        snapshotTopics: readonly string[];
    }>;
    unavailable: Readonly<{
        workspace: string[];
        containers: any[];
        images: any[];
        terminal: any[];
        events: any[];
        extensions: any[];
    }>;
}>;

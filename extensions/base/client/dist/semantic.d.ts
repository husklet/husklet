/** Deterministic bounded XML projection with explicit source and projection completeness. */
export declare function semanticText(tree: any): Readonly<{
    text: string;
    complete: boolean;
    sourceTruncated: boolean;
    projectionTruncated: false;
}>;
/** Compatibility shorthand when the caller only needs the bounded XML bytes. */
export declare function semanticXml(tree: any): string;

/** Runtime documentation identity for a component composed from native Husklet nodes. */
export interface CompositeComponentDefinition<
  Name extends string = string,
  Family extends string = string,
> {
  readonly name: Name;
  readonly family: Family;
  readonly summary: string;
}

/**
 * Public React components that are not native protocol tags.
 *
 * Storybook consumes this list so adding a composite cannot silently omit it
 * from component navigation. Product applications may use the same identity
 * without depending on Storybook.
 */
export const compositeComponents = [
  {
    name: 'ConfirmAction',
    family: 'buttons',
    summary: 'A two-stage destructive action bound to one stable authority.',
  },
  {
    name: 'ResourceState',
    family: 'feedback',
    summary: 'A mutually exclusive loading, empty, failure, or ready resource boundary.',
  },
  {
    name: 'ResourceIdentity',
    family: 'content',
    summary:
      'A compact label with the complete selectable identity required for exact resource actions.',
  },
  {
    name: 'RecoveryState',
    family: 'feedback',
    summary: 'Actionable recovery with bounded technical details disclosed on request.',
  },
  {
    name: 'TerminalTranscript',
    family: 'content',
    summary: 'A bounded, selectable native projection of terminal output.',
  },
  {
    name: 'CommandPaletteView',
    family: 'navigation',
    summary: 'A bounded keyboard-first command picker composed from native controls.',
  },
  {
    name: 'JsonTree',
    family: 'trees',
    summary: 'A cycle-safe and bounded interactive JSON tree.',
  },
  {
    name: 'ObjectInspector',
    family: 'trees',
    summary: 'The object-inspection alias for the bounded interactive JSON tree.',
  },
] as const satisfies readonly CompositeComponentDefinition[];

export type CompositeComponentName = (typeof compositeComponents)[number]['name'];

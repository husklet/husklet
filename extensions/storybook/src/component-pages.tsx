import React from 'react';
import {
  compositeComponents,
  type CompositeComponentName,
  type CompositeComponentDefinition,
} from '@husklet/react';

import { ButtonWorkbench } from './button.js';
import { AutocompleteWorkbench } from './autocomplete.js';
import { CardWorkbench } from './card.js';
import { CardActionsWorkbench } from './card-actions.js';
import { CheckboxWorkbench } from './checkbox.js';
import { CommandPaletteStory } from './command-palette.js';
import { ConfirmationStory } from './confirmation.js';
import { EntryWorkbench } from './entry.js';
import { ExpanderWorkbench } from './expander.js';
import { FormControlWorkbench } from './form-control.js';
import { HeadingWorkbench } from './heading.js';
import { IconButtonWorkbench } from './icon-button.js';
import { InlineMessageWorkbench } from './inline-message.js';
import { JsonTreeStory } from './json-tree.js';
import { RadioWorkbench } from './radio.js';
import { RadioGroupWorkbench } from './radio-group.js';
import { NumberEntryWorkbench } from './number-entry.js';
import { PasswordEntryWorkbench } from './password-entry.js';
import { SearchWorkbench } from './search.js';
import { RecoveryStateStory } from './recovery-state.js';
import { ResourceStateStory } from './resource-state.js';
import { ResourceIdentityStory } from './resource-identity.js';
import { SelectWorkbench } from './select.js';
import { SliderWorkbench } from './slider.js';
import { SwitchWorkbench } from './switch.js';
import { TextAreaWorkbench } from './text-area.js';
import { TerminalTranscriptStory } from './terminal-transcript.js';
import { ToggleButtonWorkbench } from './toggle-button.js';
import { ComponentDocument, DocumentationSection } from './component-document.js';

type Page = React.ComponentType<Record<string, never>>;
type NativeWorkbenchName =
  | 'Autocomplete'
  | 'Button'
  | 'Card'
  | 'CardActions'
  | 'Checkbox'
  | 'Entry'
  | 'Expander'
  | 'FormControl'
  | 'Heading'
  | 'IconButton'
  | 'InlineMessage'
  | 'NumberEntry'
  | 'PasswordEntry'
  | 'Radio'
  | 'RadioGroup'
  | 'Search'
  | 'Select'
  | 'Slider'
  | 'Switch'
  | 'TextArea'
  | 'ToggleButton';

const compositeExamples = {
  ConfirmAction: ConfirmationStory,
  ResourceState: ResourceStateStory,
  ResourceIdentity: ResourceIdentityStory,
  RecoveryState: RecoveryStateStory,
  TerminalTranscript: TerminalTranscriptStory,
  CommandPaletteView: CommandPaletteStory,
  JsonTree: JsonTreeStory,
  ObjectInspector: JsonTreeStory,
} satisfies Record<CompositeComponentName, Page>;

const compositeDefinitions = new Map(
  compositeComponents.map((definition) => [definition.name, definition]),
);

function documentedComposite(definition: CompositeComponentDefinition, Example: Page): Page {
  function CompositePage() {
    return (
      <ComponentDocument name={definition.name} summary={definition.summary}>
        <DocumentationSection title="Overview">
          <Example />
        </DocumentationSection>
      </ComponentDocument>
    );
  }
  return CompositePage;
}

const compositePages = Object.fromEntries(
  Object.entries(compositeExamples).map(([name, Example]) => {
    const definition = compositeDefinitions.get(name as CompositeComponentName);
    if (!definition) throw new Error(`missing composite definition for ${name}`);
    return [name, documentedComposite(definition, Example)];
  }),
) as Record<CompositeComponentName, Page>;

const nativeWorkbenchPages = {
  Autocomplete: AutocompleteWorkbench,
  Button: ButtonWorkbench,
  Card: CardWorkbench,
  CardActions: CardActionsWorkbench,
  Checkbox: CheckboxWorkbench,
  Entry: EntryWorkbench,
  Expander: ExpanderWorkbench,
  FormControl: FormControlWorkbench,
  Heading: HeadingWorkbench,
  IconButton: IconButtonWorkbench,
  InlineMessage: InlineMessageWorkbench,
  NumberEntry: NumberEntryWorkbench,
  PasswordEntry: PasswordEntryWorkbench,
  Radio: RadioWorkbench,
  RadioGroup: RadioGroupWorkbench,
  Search: SearchWorkbench,
  Select: SelectWorkbench,
  Slider: SliderWorkbench,
  Switch: SwitchWorkbench,
  TextArea: TextAreaWorkbench,
  ToggleButton: ToggleButtonWorkbench,
} satisfies Record<NativeWorkbenchName, Page>;

/** Every authored single-component page, addressed by its public component name. */
export const componentPages: Readonly<Record<string, Page>> = Object.freeze({
  ...nativeWorkbenchPages,
  ...compositePages,
});

export function componentPage(name: string): Page | undefined {
  return componentPages[name];
}

/** Creates one selected page without constructing a component type during its caller's render. */
export function renderComponentPage(name: string): React.ReactNode {
  const Page = componentPage(name);
  return Page ? React.createElement(Page) : null;
}

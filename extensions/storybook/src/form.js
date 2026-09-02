import React from 'react';
import {
  Banner,
  Button,
  Column,
  Entry,
  FormControl,
  FormControlLabel,
  FormHelperText,
  FormLabel,
  Heading,
  Row,
  Select,
  Switch,
  TagInput,
  Text,
  ToggleButton,
  ValidationSummary,
} from '@husklet/react';

const { createElement: h, useState } = React;

export const FORM_STORY = 'Validated settings form';

const environments = [
  { value: 'development', label: 'Development' },
  { value: 'production', label: 'Production' },
];

/** A complete controlled form: invalid submit, correction, and success feedback. */
export function ValidatedSettingsFormStory() {
  const [name, setName] = useState('');
  const [environment, setEnvironment] = useState('development');
  const [restart, setRestart] = useState(true);
  const [attempted, setAttempted] = useState(false);
  const [saved, setSaved] = useState(false);
  const [tag, setTag] = useState('');
  const [tags, setTags] = useState(['backend', 'managed']);
  const [reviewed, setReviewed] = useState(false);
  const invalid = attempted && name.trim().length < 3;
  const changeName = (event) => {
    setName(String(event.value ?? ''));
    setSaved(false);
  };
  const submit = () => {
    setAttempted(true);
    setSaved(name.trim().length >= 3);
  };

  return h(
    Column,
    { gap: 3, width: { maximum: { chars: 58 } } },
    h(Heading, { key: 'title', label: 'Workspace defaults', scale: 'title', wrap: true }),
    h(Text, {
      key: 'intro',
      label: 'A controlled form that validates on submit and keeps feedback beside the affected field.',
      color: 'text-dim',
      wrap: true,
    }),
    h(
      FormControl,
      { key: 'name', gap: 1 },
      h(FormLabel, { key: 'label', label: 'Workspace name' }),
      h(Entry, {
        key: 'entry',
        value: name,
        placeholder: 'api',
        tone: invalid ? 'danger' : 'neutral',
        onChange: changeName,
        onSubmit: submit,
      }),
      h(FormHelperText, {
        key: 'help',
        label: invalid ? 'Use at least 3 characters.' : 'Shown in tabs and resource lists.',
        tone: invalid ? 'danger' : 'neutral',
      }),
    ),
    h(
      FormControl,
      { key: 'environment', gap: 1 },
      h(FormLabel, { key: 'label', label: 'Environment' }),
      h(Select, {
        key: 'select',
        choices: environments,
        value: environment,
        onChange: (event) => {
          setEnvironment(String(event.value ?? 'development'));
          setSaved(false);
        },
      }),
    ),
    h(
      FormControlLabel,
      { key: 'restart', label: 'Restart after configuration changes', gap: 2 },
      h(Switch, {
        checked: restart,
        onToggle: (event) => {
          setRestart(event.value === null ? !restart : Boolean(event.value));
          setSaved(false);
        },
      }),
    ),
    h(
      FormControl,
      { key: 'tags', gap: 1 },
      h(FormLabel, { label: 'Workspace tags' }),
      h(TagInput, {
        value: tag,
        placeholder: 'Add a tag',
        gap: 1,
        onChange: (event) => setTag(String(event.value ?? '')),
        onSubmit: () => {
          const next = tag.trim();
          if (next && !tags.includes(next)) setTags([...tags, next]);
          setTag('');
        },
      }, ...tags.map((held) => h(ToggleButton, {
        key: held,
        label: held,
        checked: true,
        tooltip: `Remove ${held}`,
        onToggle: () => setTags(tags.filter((candidate) => candidate !== held)),
      }))),
      h(FormHelperText, { label: 'Press Enter to retain a tag; activate a tag to remove it.' }),
    ),
    ...(invalid
      ? [h(ValidationSummary, {
        key: 'invalid',
        label: 'Fix the highlighted field before saving.',
        detail: reviewed ? 'Workspace name is ready for correction.' : '1 problem found.',
        tone: 'danger',
      }, h(Button, { label: 'Review workspace name', onInvoke: () => setReviewed(true) }))]
      : []),
    ...(saved
      ? [h(Banner, { key: 'saved', label: `Defaults saved for ${name.trim()}.`, tone: 'positive' })]
      : []),
    h(
      Row,
      { key: 'actions', gap: 2, justify: 'end' },
      h(Button, { label: 'Save defaults', tone: 'accent', onInvoke: submit }),
    ),
  );
}

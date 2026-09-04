// @ts-nocheck -- legacy story typing is migrated incrementally.
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

const { useState } = React;

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

  return (
    <Column gap={3} width={{ maximum: { chars: 58 } }}>
      <Heading key={'title'} label={'Workspace defaults'} scale={'title'} wrap={true} />
      <Text
        key={'intro'}
        label={'A controlled form that validates on submit and keeps feedback beside the affected field.'}
        color={'text-dim'}
        wrap={true} />
      <FormControl key={'name'} gap={1}>
        <FormLabel key={'label'} label={'Workspace name'} />
        <Entry
          key={'entry'}
          value={name}
          placeholder={'api'}
          tone={invalid ? 'danger' : 'neutral'}
          onChange={changeName}
          onSubmit={submit} />
        <FormHelperText
          key={'help'}
          label={invalid ? 'Use at least 3 characters.' : 'Shown in tabs and resource lists.'}
          tone={invalid ? 'danger' : 'neutral'} />
      </FormControl>
      <FormControl key={'environment'} gap={1}>
        <FormLabel key={'label'} label={'Environment'} />
        <Select
          key={'select'}
          choices={environments}
          value={environment}
          onChange={(event) => {
            setEnvironment(String(event.value ?? 'development'));
            setSaved(false);
          }} />
      </FormControl>
      <FormControlLabel key={'restart'} label={'Restart after configuration changes'} gap={2}>
        <Switch
          checked={restart}
          onToggle={(event) => {
            setRestart(event.value === null ? !restart : Boolean(event.value));
            setSaved(false);
          }} />
      </FormControlLabel>
      <FormControl key={'tags'} gap={1}>
        <FormLabel label={'Workspace tags'} />
        <TagInput
          value={tag}
          placeholder={'Add a tag'}
          gap={1}
          onChange={(event) => setTag(String(event.value ?? ''))}
          onSubmit={() => {
            const next = tag.trim();
            if (next && !tags.includes(next)) setTags([...tags, next]);
            setTag('');
          }} />
        <Column key={'held-tags'} gap={1}>
          {tags.map((held) => <ToggleButton
            key={held}
            label={held}
            checked={true}
            tooltip={`Remove ${held}`}
            onToggle={() => setTags(tags.filter((candidate) => candidate !== held))} />)}
        </Column>
        <FormHelperText label={'Press Enter to retain a tag; activate a tag to remove it.'} />
      </FormControl>
      {invalid
        ? [<ValidationSummary
        key={'invalid'}
        label={'Fix workspace name.'}
        detail={reviewed ? 'Ready to correct.' : '1 problem.'}
        tone={'danger'} />, <Button
        key={'review'}
        label={'Review workspace name'}
        onInvoke={() => setReviewed(true)} />]
        : []}
      {saved
        ? [<Banner
        key={'saved'}
        label={`Defaults saved for ${name.trim()}.`}
        tone={'positive'} />]
        : []}
      <Row key={'actions'} gap={2} justify={'end'}>
        <Button label={'Save defaults'} tone={'accent'} onInvoke={submit} />
      </Row>
    </Column>
  );
}

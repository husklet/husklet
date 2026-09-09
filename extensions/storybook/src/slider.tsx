import React from 'react';
import {
  Code,
  FormControl,
  FormHelperText,
  FormLabel,
  InlineMessage,
  Slider,
  Text,
} from '@husklet/react';
import {
  ApiReference,
  ComponentDocument,
  DocumentationSection,
  SpecimenGrid,
} from './component-document.js';
import { rows } from './editors.js';

const MIN = 0;
const MAX = 100;
const STEP = 5;

export function boundedStep(value: unknown) {
  const number = typeof value === 'number' && Number.isFinite(value) ? value : MIN;
  return Math.min(MAX, Math.max(MIN, Math.round(number / STEP) * STEP));
}

export function SliderWorkbench() {
  const [value, setValue] = React.useState(40);
  return (
    <ComponentDocument
      name="Slider"
      summary="Slider chooses one numeric value from a bounded continuous range."
    >
      <DocumentationSection title="Overview">
        <FormControl gap={1}>
          <FormLabel label={`Build cache · ${value}%`} />
          <Slider
            value={value}
            minimum={MIN}
            maximum={MAX}
            step={STEP}
            tooltip="Build cache allocation"
            onChange={(report) => setValue(boundedStep(report.value))}
          />
          <FormHelperText label="Use Left/Right arrows for one step; Home and End jump to the bounds." />
        </FormControl>
        <InlineMessage label={`Current value: ${value}%`} tone="neutral" />
        <Code
          value="<Slider value={value} minimum={0} maximum={100} step={5} onChange={setValue} />"
          wrap
        />
      </DocumentationSection>

      <DocumentationSection title="Range states">
        <SpecimenGrid>
          <FormControl gap={1}>
            <FormLabel label="Minimum · 0" />
            <Slider value={0} minimum={0} maximum={100} step={5} />
          </FormControl>
          <FormControl gap={1}>
            <FormLabel label="Middle · 50" />
            <Slider value={50} minimum={0} maximum={100} step={5} />
          </FormControl>
          <FormControl gap={1}>
            <FormLabel label="Maximum · 100" />
            <Slider value={100} minimum={0} maximum={100} step={5} />
          </FormControl>
          <FormControl gap={1}>
            <FormLabel label="Disabled · 35" />
            <Slider value={35} minimum={0} maximum={100} step={5} enabled={false} />
          </FormControl>
        </SpecimenGrid>
      </DocumentationSection>

      <DocumentationSection title="Step sizes">
        <SpecimenGrid>
          <FormControl gap={1}>
            <FormLabel label="Coarse · step 25" />
            <Slider value={50} minimum={0} maximum={100} step={25} />
          </FormControl>
          <FormControl gap={1}>
            <FormLabel label="Fine · step 0.1" />
            <Slider value={0.5} minimum={0} maximum={1} step={0.1} />
          </FormControl>
        </SpecimenGrid>
      </DocumentationSection>

      <DocumentationSection title="Keyboard and accessibility">
        <Text
          label="Give every Slider a visible label that includes its current value. Arrow keys change one declared step; Home selects minimum and End selects maximum. Disabled sliders neither move nor report changes."
          wrap
        />
      </DocumentationSection>
      <DocumentationSection title="API">
        <ApiReference rows={rows('Slider')} />
      </DocumentationSection>
    </ComponentDocument>
  );
}

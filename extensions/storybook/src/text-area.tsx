import React from 'react';
import { Code, Text, TextArea } from '@husklet/react';
import {
  ApiReference,
  ComponentDocument,
  DocumentationSection,
  FieldSpecimen,
  SpecimenGrid,
} from './component-document.js';
import { rows } from './editors.js';

export function TextAreaWorkbench() {
  const [state, setState] = React.useState({
    value: 'build:\n  command: npm test',
    feedback: 'Edit the manifest to inspect its Change event.',
  });

  return (
    <ComponentDocument
      name="TextArea"
      summary="TextArea edits bounded multi-line content. Choose prose or monospace presentation from the content, and keep validation beside the editor."
    >
      <DocumentationSection title="Overview">
        <FieldSpecimen label="Task manifest" helper={state.feedback} width={{ chars: 48 }}>
          <TextArea
            value={state.value}
            monospace
            tooltip="Task manifest"
            width={{ chars: 48 }}
            height={{ step: 28 }}
            onChange={(report) => {
              const next = String(report.value ?? '').slice(0, 2048);
              setState({
                value: next,
                feedback: `${next.split('\n').length} lines · ${next.length} characters`,
              });
            }}
          />
        </FieldSpecimen>
        <Code value={'<TextArea value={manifest} monospace onChange={setManifest} />'} wrap />
      </DocumentationSection>

      <DocumentationSection title="Presentation">
        <SpecimenGrid>
          <FieldSpecimen
            label="Prose"
            helper="Word wrapping follows the available width"
            width={{ chars: 36 }}
          >
            <TextArea
              value="Describe why this extension needs workspace access and how a reviewer can verify it."
              monospace={false}
              width={{ chars: 36 }}
              height={{ step: 24 }}
            />
          </FieldSpecimen>
          <FieldSpecimen
            label="Code and configuration"
            helper="Monospace preserves structural alignment"
            width={{ chars: 36 }}
          >
            <TextArea
              value={'limits:\n  memory: 512 MiB'}
              monospace
              width={{ chars: 36 }}
              height={{ step: 24 }}
            />
          </FieldSpecimen>
        </SpecimenGrid>
      </DocumentationSection>

      <DocumentationSection title="States">
        <SpecimenGrid>
          <FieldSpecimen
            label="Empty"
            helper="Pair an empty editor with visible instructions"
            width={{ chars: 36 }}
          >
            <TextArea value="" width={{ chars: 36 }} height={{ step: 24 }} />
          </FieldSpecimen>
          <FieldSpecimen
            label="Disabled"
            helper="Keep unavailable content readable"
            width={{ chars: 36 }}
          >
            <TextArea
              value="Generated after the first successful run."
              enabled={false}
              width={{ chars: 36 }}
              height={{ step: 24 }}
            />
          </FieldSpecimen>
          <FieldSpecimen label="Invalid" helper="A command is required" width={{ chars: 36 }}>
            <TextArea
              value={'build:\n  command:'}
              monospace
              tone="danger"
              width={{ chars: 36 }}
              height={{ step: 24 }}
            />
          </FieldSpecimen>
        </SpecimenGrid>
      </DocumentationSection>

      <DocumentationSection title="Behavior">
        <Text
          label="TextArea reports the complete controlled value after each edit. Bound retained text, preserve line breaks, and show validation without replacing the editor contents."
          wrap
        />
      </DocumentationSection>

      <DocumentationSection title="Accessibility">
        <Text
          label="Provide a persistent visible label. Tab moves focus into the native multi-line editor; arrow keys move its caret, while the surrounding page remains scrollable. Disabled content must remain named and legible."
          wrap
        />
      </DocumentationSection>

      <DocumentationSection title="API">
        <ApiReference rows={rows('TextArea')} />
      </DocumentationSection>
    </ComponentDocument>
  );
}

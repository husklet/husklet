import React from 'react';
import { Code, Search, Text } from '@husklet/react';
import {
  ApiReference,
  ComponentDocument,
  DocumentationSection,
  FieldSpecimen,
  SpecimenGrid,
} from './component-document.js';
import { rows } from './editors.js';

export function SearchWorkbench() {
  const [state, setState] = React.useState({
    query: 'network',
    event: 'Edit the query or use its clear affordance.',
  });

  return (
    <ComponentDocument
      name="Search"
      summary="Search narrows an existing collection as a query changes and provides a native way to clear it."
    >
      <DocumentationSection title="Overview">
        <FieldSpecimen label="Find extensions" helper={state.event} width={{ chars: 30 }}>
          <Search
            value={state.query}
            placeholder="Name, status, or provider"
            tooltip="Find extensions"
            width={{ chars: 30 }}
            onChange={(report) => {
              const next = String(report.value ?? '').slice(0, 128);
              setState({
                query: next,
                event: next ? `Filtering by “${next}”.` : 'Showing every extension.',
              });
            }}
          />
        </FieldSpecimen>
        <Code
          value={
            '<Search value={query} placeholder="Name, status, or provider" onChange={setQuery} />'
          }
          wrap
        />
      </DocumentationSection>

      <DocumentationSection title="States">
        <SpecimenGrid>
          <FieldSpecimen label="Empty" helper="No filter is applied" width={{ chars: 30 }}>
            <Search value="" placeholder="Search extensions" width={{ chars: 30 }} />
          </FieldSpecimen>
          <FieldSpecimen
            label="Query"
            helper="The clear affordance remains available"
            width={{ chars: 30 }}
          >
            <Search value="faulted" width={{ chars: 30 }} />
          </FieldSpecimen>
          <FieldSpecimen
            label="Focused"
            helper="Typing updates results immediately"
            width={{ chars: 30 }}
          >
            <Search value="network" tooltip="Focused extension search" width={{ chars: 30 }} />
          </FieldSpecimen>
          <FieldSpecimen
            label="Disabled"
            helper="Explain why filtering is unavailable"
            width={{ chars: 30 }}
          >
            <Search value="workspace" enabled={false} width={{ chars: 30 }} />
          </FieldSpecimen>
        </SpecimenGrid>
      </DocumentationSection>

      <DocumentationSection title="Behavior">
        <Text
          label="Filter as the query changes; do not require a submit button. Preserve the query while results refresh, reset pagination when it changes, and show the unfiltered collection after clearing."
          wrap
        />
      </DocumentationSection>

      <DocumentationSection title="Accessibility">
        <Text
          label="Give the search a visible label and a matching accessible name. A placeholder describes searchable terms but does not replace the label. Keep result counts and empty results adjacent to the field."
          wrap
        />
      </DocumentationSection>

      <DocumentationSection title="API">
        <ApiReference rows={rows('Search')} />
      </DocumentationSection>
    </ComponentDocument>
  );
}

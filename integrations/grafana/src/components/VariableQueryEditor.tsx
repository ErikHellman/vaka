import React from 'react';
import { Combobox, ComboboxOption, InlineField, Stack } from '@grafana/ui';
import { QueryEditorProps } from '@grafana/data';

import type { DataSource } from '../datasource';
import { QueryType, VakaOptions, VakaQuery } from '../types';
import { DIMENSIONS, ProjectPicker } from './common';

type Props = QueryEditorProps<DataSource, VakaQuery, VakaOptions>;

const KINDS: Array<ComboboxOption<string>> = [
  { label: 'Projects', value: QueryType.VarProjects },
  { label: 'Releases', value: QueryType.VarReleases },
  { label: 'Event names', value: QueryType.VarEventNames },
  {
    label: 'Values of a dimension',
    value: QueryType.VarDimensionValues,
    description: 'Top 100 values in the time range',
  },
];

export function VariableQueryEditor({ query, onChange, datasource }: Props) {
  const kind = query.queryType ?? QueryType.VarProjects;
  const set = (patch: Partial<VakaQuery>) => onChange({ ...query, ...patch });
  return (
    <Stack direction="row" wrap="wrap" gap={1}>
      <InlineField label="Values" labelWidth={12}>
        <Combobox options={KINDS} value={kind} width={28} onChange={(o) => set({ queryType: o.value as QueryType })} />
      </InlineField>
      {kind !== QueryType.VarProjects && (
        <ProjectPicker datasource={datasource} value={query.project} onChange={(project) => set({ project })} />
      )}
      {kind === QueryType.VarDimensionValues && (
        <InlineField label="Dimension" labelWidth={12}>
          <Combobox
            options={DIMENSIONS}
            value={query.dimension ?? null}
            createCustomValue
            width={24}
            onChange={(o) => set({ dimension: o.value })}
          />
        </InlineField>
      )}
    </Stack>
  );
}

import React, { useCallback } from 'react';
import { Button, Combobox, ComboboxOption, IconButton, InlineField, Input, Stack } from '@grafana/ui';
import { getTemplateSrv } from '@grafana/runtime';

import type { DataSource } from '../datasource';
import { Filter } from '../types';

export const opt = (value: string, label?: string, description?: string): ComboboxOption<string> => ({
  value,
  label: label ?? value,
  description,
});

export const DIMENSIONS: Array<ComboboxOption<string>> = [
  'path',
  'url',
  'hostname',
  'referrer',
  'referrer_url',
  'utm_source',
  'utm_medium',
  'utm_campaign',
  'utm_term',
  'utm_content',
  'country',
  'device',
  'device_model',
  'browser',
  'browser_version',
  'os',
  'os_version',
  'app_version',
  'release',
  'environment',
  'platform',
  'event',
  'type',
  'screen',
].map((d) => opt(d));

export const FILTER_KEYS: Array<ComboboxOption<string>> = [
  ...DIMENSIONS,
  opt('cohort', 'cohort', 'Saved cohort name; use != to exclude'),
];

function variableOptions(): Array<ComboboxOption<string>> {
  try {
    return getTemplateSrv()
      .getVariables()
      .map((v) => opt(`$${v.name}`, `$${v.name}`, 'Dashboard variable'));
  } catch {
    return [];
  }
}

/** Loads options from a backend resource each time the dropdown is searched. */
function useResourceOptions(
  datasource: DataSource,
  path: string,
  params: Record<string, string | undefined>,
  extra: Array<ComboboxOption<string>> = []
) {
  const key = JSON.stringify(params);
  const extraKey = JSON.stringify(extra);
  return useCallback(
    async (input: string) => {
      const res = await datasource.options(path, JSON.parse(key));
      const all = [
        ...(JSON.parse(extraKey) as Array<ComboboxOption<string>>),
        ...res.map((o) => opt(o.value, o.text === o.value ? o.value : `${o.text} (${o.value})`)),
      ];
      const needle = input.toLowerCase();
      return all.filter((o) => !needle || (o.label ?? o.value).toLowerCase().includes(needle));
    },
    [datasource, path, key, extraKey]
  );
}

export function ProjectPicker(props: { datasource: DataSource; value?: string; onChange: (v: string) => void }) {
  const load = useResourceOptions(props.datasource, 'projects', {}, variableOptions());
  return (
    <InlineField label="Project" labelWidth={12} required>
      <Combobox
        options={load}
        value={props.value || props.datasource.defaultProject || null}
        createCustomValue
        width={28}
        placeholder="Choose a project"
        data-testid="vaka-project"
        onChange={(o) => props.onChange(o.value)}
      />
    </InlineField>
  );
}

export function ResourcePicker(props: {
  datasource: DataSource;
  label: string;
  resource: string;
  project?: string;
  value?: string;
  placeholder?: string;
  tooltip?: string;
  width?: number;
  extra?: Array<ComboboxOption<string>>;
  onChange: (v: string) => void;
}) {
  const load = useResourceOptions(props.datasource, props.resource, { project: props.project }, [
    ...(props.extra ?? []),
    ...variableOptions(),
  ]);
  return (
    <InlineField label={props.label} labelWidth={12} tooltip={props.tooltip}>
      <Combobox
        options={load}
        value={props.value || null}
        createCustomValue
        isClearable
        width={props.width ?? 24}
        placeholder={props.placeholder ?? 'Any'}
        onChange={(o) => props.onChange(o?.value ?? '')}
      />
    </InlineField>
  );
}

export function TextField(props: {
  label: string;
  value?: string;
  placeholder?: string;
  tooltip?: string;
  width?: number;
  onChange: (v: string) => void;
}) {
  return (
    <InlineField label={props.label} labelWidth={12} tooltip={props.tooltip}>
      <Input
        value={props.value ?? ''}
        placeholder={props.placeholder}
        width={props.width ?? 24}
        onChange={(e) => props.onChange(e.currentTarget.value)}
      />
    </InlineField>
  );
}

export function Choice(props: {
  label: string;
  options: Array<ComboboxOption<string>>;
  value?: string;
  tooltip?: string;
  width?: number;
  clearable?: boolean;
  placeholder?: string;
  onChange: (v: string) => void;
}) {
  return (
    <InlineField label={props.label} labelWidth={12} tooltip={props.tooltip}>
      {props.clearable ? (
        <Combobox
          options={props.options}
          value={props.value || null}
          isClearable
          createCustomValue
          placeholder={props.placeholder}
          width={props.width ?? 24}
          onChange={(o) => props.onChange(o?.value ?? '')}
        />
      ) : (
        <Combobox
          options={props.options}
          value={props.value || null}
          width={props.width ?? 24}
          onChange={(o) => props.onChange(o.value)}
        />
      )}
    </InlineField>
  );
}

const OPS: Array<ComboboxOption<string>> = [opt('=', '='), opt('!=', '!='), opt('=~', 'contains')];

export function FiltersEditor(props: { filters?: Filter[]; onChange: (f: Filter[]) => void }) {
  const filters = props.filters ?? [];
  const update = (i: number, patch: Partial<Filter>) =>
    props.onChange(filters.map((f, j) => (i === j ? { ...f, ...patch } : f)));
  return (
    <Stack direction="column" gap={0.5}>
      {filters.map((f, i) => (
        <Stack key={i} direction="row" gap={0.5} alignItems="center">
          <InlineField label={i === 0 ? 'Filters' : ''} labelWidth={12}>
            <Combobox
              options={FILTER_KEYS}
              value={f.key || null}
              createCustomValue
              width={20}
              placeholder="dimension or prop:key"
              onChange={(o) => update(i, { key: o.value })}
            />
          </InlineField>
          <Combobox
            options={OPS}
            value={f.op || '='}
            width={12}
            onChange={(o) => update(i, { op: o.value as Filter['op'] })}
          />
          <Input
            value={f.value}
            width={24}
            placeholder="value or $variable"
            onChange={(e) => update(i, { value: e.currentTarget.value })}
          />
          <IconButton
            name="trash-alt"
            tooltip="Remove filter"
            onClick={() => props.onChange(filters.filter((_, j) => j !== i))}
          />
        </Stack>
      ))}
      <div>
        <Button
          size="sm"
          variant="secondary"
          icon="plus"
          onClick={() => props.onChange([...filters, { key: '', op: '=', value: '' }])}
        >
          Add filter
        </Button>
      </div>
    </Stack>
  );
}

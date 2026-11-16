import React from 'react';
import { Combobox, ComboboxOption, InlineField, InlineSwitch, Input, MultiCombobox, Stack } from '@grafana/ui';
import { QueryEditorProps } from '@grafana/data';

import type { DataSource } from '../datasource';
import { QueryType, VakaOptions, VakaQuery } from '../types';
import { Choice, DIMENSIONS, FiltersEditor, ProjectPicker, ResourcePicker, TextField, opt } from './common';

type Props = QueryEditorProps<DataSource, VakaQuery, VakaOptions>;

export const QUERY_TYPES: Array<ComboboxOption<string>> = [
  opt(
    QueryType.ErrorsTimeseries,
    'Errors over time',
    'Error counts per bucket, optionally per issue or release. Alertable'
  ),
  opt(QueryType.Issues, 'Issues', 'Table of issues with event and user counts'),
  opt(
    QueryType.SessionsTimeseries,
    'Sessions and crash-free rate',
    'Sessions and crash-free rates over time, optionally per release. Alertable'
  ),
  opt(QueryType.ReleaseHealth, 'Release health', 'Crash-free sessions and users per release'),
  opt(QueryType.ReleaseAdoption, 'Release adoption', 'Share of sessions per release over time'),
  opt(QueryType.AnalyticsTimeseries, 'Analytics over time', 'Visitors, pageviews and events over time. Alertable'),
  opt(
    QueryType.AnalyticsBreakdown,
    'Analytics breakdown',
    'Top values of a dimension: pages, referrers, countries, ...'
  ),
  opt(QueryType.AnalyticsSummary, 'Analytics summary', 'Totals for the time range with the previous period'),
  opt(QueryType.Realtime, 'Realtime visitors', 'The last few minutes'),
  opt(QueryType.Funnel, 'Funnel', 'Conversion through a sequence of events'),
  opt(QueryType.PerfTimeseries, 'Performance over time', 'Latency percentiles and throughput. Alertable'),
  opt(QueryType.PerfSummary, 'Performance summary', 'Latency per transaction'),
  opt(QueryType.WebVitals, 'Web Vitals', 'LCP, INP, CLS, FCP and TTFB at p75'),
  opt(QueryType.Logs, 'Logs', 'Logs sent over OpenTelemetry'),
  opt(QueryType.Trace, 'Trace', 'One trace as a span tree'),
  opt(QueryType.Monitors, 'Monitors', 'Heartbeat, HTTP and TLS monitors with uptime'),
  opt(QueryType.Incidents, 'Incidents', 'Monitor incidents'),
  opt(QueryType.Live, 'Live stream', 'New errors, events or spans as they arrive'),
  opt(QueryType.AnnReleases, 'Annotations: releases', 'A marker when each release was first seen'),
  opt(QueryType.AnnIssues, 'Annotations: new issues', 'New issues and regressions'),
  opt(QueryType.AnnIncidents, 'Annotations: incidents', 'Monitor incidents as regions'),
];

const ANALYTICS_METRICS = ['visitors', 'pageviews', 'screen_views', 'custom_events', 'events', 'identified_users'];
const ERROR_METRICS = ['errors', 'unhandled', 'fatal', 'users', 'issues'];
const SESSION_METRICS = [
  'crash_free_sessions',
  'crash_free_users',
  'sessions',
  'crashed',
  'errored',
  'users',
  'crashed_users',
];
const PERF_METRICS = ['p50_ms', 'p75_ms', 'p95_ms', 'p99_ms', 'avg_ms', 'count', 'per_minute', 'errors', 'error_rate'];
const ADOPTION_METRICS = ['share_sessions', 'share_users', 'sessions', 'users'];

const ERROR_GROUPS = [
  'issue',
  'release',
  'level',
  'environment',
  'exception_type',
  'platform',
  'os',
  'browser',
  'country',
];
const PERF_GROUPS = ['transaction', 'op', 'release', 'environment', 'service', 'http_status'];
const SESSION_GROUPS = ['release', 'environment', 'platform'];

const EVENT_TYPES = [opt('pageview'), opt('screen_view'), opt('custom')];
const LEVELS = ['fatal', 'error', 'warning', 'info', 'debug'].map((l) => opt(l));
const SEVERITIES = ['trace', 'debug', 'info', 'warn', 'error', 'fatal'].map((l) => opt(l, l, `${l} and above`));
const BUCKETS = [
  opt('', 'auto', "Follow the panel's interval"),
  ...['1m', '5m', '15m', '30m', '1h', '6h', '1d', '1w'].map((b) => opt(b)),
];

function MetricsPicker(props: {
  options: string[];
  value?: string[];
  defaults: string[];
  single?: boolean;
  onChange: (m: string[]) => void;
}) {
  const value = props.value && props.value.length > 0 ? props.value : props.defaults;
  const options = props.options.map((m) => opt(m));
  return (
    <InlineField
      label={props.single ? 'Metric' : 'Metrics'}
      labelWidth={12}
      tooltip={props.single ? undefined : 'With Group by, only the first metric is shown, one series per group'}
    >
      {props.single ? (
        <Combobox options={options} value={value[0]} width={24} onChange={(o) => props.onChange([o.value])} />
      ) : (
        <MultiCombobox
          options={options}
          value={value}
          width="auto"
          minWidth={24}
          onChange={(o) => props.onChange(o.map((x) => x.value))}
        />
      )}
    </InlineField>
  );
}

export function QueryEditor({ query, onChange, onRunQuery, datasource }: Props) {
  const qt = query.queryType ?? QueryType.ErrorsTimeseries;
  const set = (patch: Partial<VakaQuery>, run = true) => {
    onChange({ ...query, ...patch });
    if (run) {
      onRunQuery();
    }
  };
  const project = query.project || datasource.defaultProject;

  const groupBy = (groups: string[]) => (
    <>
      <Choice
        label="Group by"
        options={groups.map((g) => opt(g))}
        value={query.groupBy}
        clearable
        placeholder="None"
        onChange={(groupBy) => set({ groupBy })}
      />
      {query.groupBy && (
        <TextField
          label="Top"
          width={8}
          value={query.limit ? String(query.limit) : ''}
          placeholder="10"
          tooltip="Number of groups"
          onChange={(v) => set({ limit: parseInt(v, 10) || undefined })}
        />
      )}
    </>
  );
  const bucket = (
    <Choice
      label="Bucket"
      options={BUCKETS}
      value={query.bucket ?? ''}
      width={12}
      onChange={(b) => set({ bucket: b })}
    />
  );
  const release = (
    <ResourcePicker
      datasource={datasource}
      label="Release"
      resource="releases"
      project={project}
      value={query.release}
      onChange={(release) => set({ release })}
    />
  );
  const environment = (
    <TextField
      label="Environment"
      value={query.environment}
      placeholder="Any"
      onChange={(environment) => set({ environment }, false)}
    />
  );
  const eventType = (
    <Choice
      label="Event type"
      options={EVENT_TYPES}
      value={query.eventType}
      clearable
      placeholder="All"
      onChange={(eventType) => set({ eventType })}
    />
  );
  const filters = <FiltersEditor filters={query.filters} onChange={(f) => set({ filters: f })} />;

  let fields: React.ReactNode = null;
  switch (qt) {
    case QueryType.AnalyticsTimeseries:
      fields = (
        <>
          <MetricsPicker
            options={ANALYTICS_METRICS}
            defaults={['visitors', 'pageviews']}
            value={query.metrics}
            onChange={(metrics) => set({ metrics })}
          />
          {groupBy(DIMENSIONS.map((d) => d.value))}
          {eventType}
          {bucket}
        </>
      );
      break;
    case QueryType.AnalyticsBreakdown:
      fields = (
        <>
          <Choice
            label="Dimension"
            options={DIMENSIONS}
            value={query.dimension}
            clearable
            placeholder="path, country, prop:plan"
            onChange={(dimension) => set({ dimension })}
          />
          <MetricsPicker
            options={ANALYTICS_METRICS}
            defaults={['visitors', 'pageviews', 'events']}
            value={query.metrics}
            onChange={(metrics) => set({ metrics })}
          />
          <TextField
            label="Limit"
            width={8}
            value={query.limit ? String(query.limit) : ''}
            placeholder="10"
            onChange={(v) => set({ limit: parseInt(v, 10) || undefined })}
          />
          {eventType}
        </>
      );
      break;
    case QueryType.AnalyticsSummary:
      fields = (
        <>
          <MetricsPicker
            options={ANALYTICS_METRICS}
            defaults={ANALYTICS_METRICS}
            value={query.metrics}
            onChange={(metrics) => set({ metrics })}
          />
          {eventType}
        </>
      );
      break;
    case QueryType.Realtime:
      fields = (
        <TextField
          label="Minutes"
          width={8}
          value={query.window}
          placeholder="5"
          tooltip="1 to 60"
          onChange={(window) => set({ window })}
        />
      );
      break;
    case QueryType.ErrorsTimeseries:
      fields = (
        <>
          <MetricsPicker
            options={ERROR_METRICS}
            defaults={['errors']}
            value={query.metrics}
            onChange={(metrics) => set({ metrics })}
          />
          {groupBy(ERROR_GROUPS)}
          {bucket}
          <TextField
            label="Issue"
            value={query.issue}
            placeholder="WEB-12"
            onChange={(issue) => set({ issue }, false)}
          />
          <Choice
            label="Level"
            options={LEVELS}
            value={query.level}
            clearable
            placeholder="Any"
            onChange={(level) => set({ level })}
          />
          {release}
          {environment}
          <TextField
            label="Exception"
            value={query.exceptionType}
            placeholder="TypeError"
            onChange={(exceptionType) => set({ exceptionType }, false)}
          />
          <Choice
            label="Handled"
            options={[opt('false', 'Unhandled only'), opt('true', 'Handled only')]}
            value={query.handled}
            clearable
            placeholder="Both"
            onChange={(handled) => set({ handled })}
          />
        </>
      );
      break;
    case QueryType.Issues:
      fields = (
        <>
          <Choice
            label="Status"
            options={['open', 'resolved', 'ignored', 'all'].map((s) => opt(s))}
            value={query.status || 'open'}
            width={14}
            onChange={(status) => set({ status })}
          />
          <Choice
            label="Sort"
            options={['events', 'users', 'last_seen', 'first_seen', 'times_seen'].map((s) => opt(s))}
            value={query.sort || 'last_seen'}
            width={14}
            onChange={(sort) => set({ sort })}
          />
          <TextField
            label="Search"
            value={query.search}
            placeholder="title or culprit"
            onChange={(search) => set({ search }, false)}
          />
          {release}
          {environment}
          <TextField
            label="Limit"
            width={8}
            value={query.limit ? String(query.limit) : ''}
            placeholder="50"
            onChange={(v) => set({ limit: parseInt(v, 10) || undefined })}
          />
        </>
      );
      break;
    case QueryType.SessionsTimeseries:
      fields = (
        <>
          <MetricsPicker
            options={SESSION_METRICS}
            defaults={['crash_free_sessions']}
            value={query.metrics}
            onChange={(metrics) => set({ metrics })}
          />
          {groupBy(SESSION_GROUPS)}
          {bucket}
          {release}
          {environment}
        </>
      );
      break;
    case QueryType.ReleaseHealth:
      fields = release;
      break;
    case QueryType.ReleaseAdoption:
      fields = (
        <>
          <MetricsPicker
            single
            options={ADOPTION_METRICS}
            defaults={['share_sessions']}
            value={query.metrics}
            onChange={(metrics) => set({ metrics })}
          />
          {environment}
          <TextField
            label="Releases"
            width={8}
            value={query.limit ? String(query.limit) : ''}
            placeholder="5"
            tooltip="Releases shown separately; the rest are 'other'"
            onChange={(v) => set({ limit: parseInt(v, 10) || undefined })}
          />
        </>
      );
      break;
    case QueryType.PerfTimeseries:
      fields = (
        <>
          <MetricsPicker
            options={PERF_METRICS}
            defaults={['p50_ms', 'p95_ms']}
            value={query.metrics}
            onChange={(metrics) => set({ metrics })}
          />
          {groupBy(PERF_GROUPS)}
          {bucket}
          <TextField
            label="Transaction"
            value={query.transaction}
            placeholder="GET /api/users/:id"
            onChange={(transaction) => set({ transaction }, false)}
          />
          <TextField
            label="Op"
            value={query.op}
            placeholder="http.server, db*"
            tooltip="With an op, every span with that op counts, not only transactions. A trailing * matches a prefix"
            onChange={(op) => set({ op }, false)}
          />
          {release}
          {environment}
        </>
      );
      break;
    case QueryType.PerfSummary:
      fields = (
        <>
          <TextField label="Op" value={query.op} placeholder="http.server" onChange={(op) => set({ op }, false)} />
          <Choice
            label="Sort"
            options={['p95', 'p50', 'p99', 'count', 'errors', 'total'].map((s) => opt(s))}
            value={query.sort || 'p95'}
            width={12}
            onChange={(sort) => set({ sort })}
          />
          {release}
          {environment}
        </>
      );
      break;
    case QueryType.WebVitals:
      fields = (
        <>
          <TextField
            label="Page"
            value={query.transaction}
            placeholder="All pages"
            onChange={(transaction) => set({ transaction }, false)}
          />
          <InlineField label="Per page" labelWidth={12}>
            <InlineSwitch value={!!query.byPage} onChange={(e) => set({ byPage: e.currentTarget.checked })} />
          </InlineField>
          {release}
          {environment}
        </>
      );
      break;
    case QueryType.Logs:
      fields = (
        <>
          <TextField
            label="Search"
            value={query.search}
            placeholder="text in the body"
            onChange={(search) => set({ search }, false)}
          />
          <Choice
            label="Severity"
            options={SEVERITIES}
            value={query.severity}
            clearable
            placeholder="Any"
            onChange={(severity) => set({ severity })}
          />
          <TextField
            label="Service"
            value={query.service}
            placeholder="Any"
            onChange={(service) => set({ service }, false)}
          />
          <TextField
            label="Trace ID"
            value={query.traceId}
            placeholder="Any"
            onChange={(traceId) => set({ traceId }, false)}
          />
          {release}
          {environment}
          <TextField
            label="Limit"
            width={8}
            value={query.limit ? String(query.limit) : ''}
            placeholder="500"
            onChange={(v) => set({ limit: parseInt(v, 10) || undefined })}
          />
        </>
      );
      break;
    case QueryType.Trace:
      fields = (
        <TextField
          label="Trace ID"
          width={40}
          value={query.traceId}
          placeholder="4bf92f3577b34da6a3ce929d0e0e4736"
          onChange={(traceId) => set({ traceId }, false)}
        />
      );
      break;
    case QueryType.Funnel:
      fields = (
        <>
          <InlineField
            label="Steps"
            labelWidth={12}
            tooltip="Comma-separated, in order: signup, pageview|path:/pricing, purchase|prop:plan:pro"
            grow
          >
            <Input
              value={(query.steps ?? []).join(', ')}
              placeholder="pageview|path:/pricing, signup, purchase"
              onChange={(e) => set({ steps: e.currentTarget.value.split(',').map((s) => s.trim()) }, false)}
              onBlur={onRunQuery}
            />
          </InlineField>
          <TextField
            label="Window"
            width={10}
            value={query.window}
            placeholder="1d"
            tooltip="Max time from first to last step"
            onChange={(window) => set({ window }, false)}
          />
        </>
      );
      break;
    case QueryType.Live:
      fields = (
        <>
          <Choice
            label="Stream"
            options={[
              opt('errors', 'Errors'),
              opt('events', 'Analytics events'),
              opt('spans', 'Transactions (root spans)'),
            ]}
            value={query.stream || 'errors'}
            width={28}
            onChange={(stream) => set({ stream: stream as VakaQuery['stream'] })}
          />
          {environment}
          <TextField
            label="Keep"
            width={8}
            value={query.limit ? String(query.limit) : ''}
            placeholder="500"
            tooltip="Rows kept in the panel"
            onChange={(v) => set({ limit: parseInt(v, 10) || undefined })}
          />
        </>
      );
      break;
    case QueryType.AnnIssues:
      fields = (
        <>
          {release}
          {environment}
        </>
      );
      break;
  }

  const showFilters =
    qt === QueryType.AnalyticsTimeseries ||
    qt === QueryType.AnalyticsBreakdown ||
    qt === QueryType.AnalyticsSummary ||
    qt === QueryType.Funnel;

  return (
    <Stack direction="column" gap={1}>
      <Stack direction="row" wrap="wrap" gap={1}>
        <InlineField label="Query" labelWidth={12}>
          <Combobox
            options={QUERY_TYPES}
            value={qt}
            width={32}
            data-testid="vaka-query-type"
            onChange={(o) => set({ queryType: o.value as QueryType, metrics: undefined, groupBy: undefined })}
          />
        </InlineField>
        <ProjectPicker datasource={datasource} value={query.project} onChange={(p) => set({ project: p })} />
      </Stack>
      <Stack direction="row" wrap="wrap" gap={1}>
        {fields}
      </Stack>
      {showFilters && filters}
    </Stack>
  );
}

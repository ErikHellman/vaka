import { DataSourceJsonData } from '@grafana/data';
import { DataQuery } from '@grafana/schema';

/** Query types; keep in sync with pkg/plugin/query.go. */
export enum QueryType {
  AnalyticsTimeseries = 'analytics_timeseries',
  AnalyticsBreakdown = 'analytics_breakdown',
  AnalyticsSummary = 'analytics_summary',
  Realtime = 'realtime',
  ErrorsTimeseries = 'errors_timeseries',
  Issues = 'issues',
  SessionsTimeseries = 'sessions_timeseries',
  ReleaseHealth = 'release_health',
  ReleaseAdoption = 'release_adoption',
  PerfTimeseries = 'performance_timeseries',
  PerfSummary = 'performance_summary',
  WebVitals = 'web_vitals',
  Logs = 'logs',
  Trace = 'trace',
  Funnel = 'funnel',
  Monitors = 'monitors',
  Incidents = 'incidents',
  Live = 'live',

  VarProjects = 'var_projects',
  VarReleases = 'var_releases',
  VarDimensionValues = 'var_dimension_values',
  VarEventNames = 'var_event_names',

  AnnReleases = 'ann_releases',
  AnnIncidents = 'ann_incidents',
  AnnIssues = 'ann_issues',
}

export interface Filter {
  key: string;
  op: '=' | '!=' | '=~';
  value: string;
}

export interface VakaQuery extends DataQuery {
  queryType?: QueryType;
  project?: string;

  metrics?: string[];
  groupBy?: string;
  limit?: number;
  /** Explicit bucket size such as 5m or 1h; empty means the panel interval. */
  bucket?: string;

  dimension?: string;
  eventType?: string;
  filters?: Filter[];
  steps?: string[];
  window?: string;

  issue?: string;
  level?: string;
  release?: string;
  environment?: string;
  exceptionType?: string;
  handled?: string;
  status?: string;
  sort?: string;
  search?: string;
  op?: string;
  transaction?: string;
  byPage?: boolean;

  severity?: string;
  service?: string;
  traceId?: string;

  stream?: 'errors' | 'events' | 'spans';
}

export const DEFAULT_QUERY: Partial<VakaQuery> = {
  queryType: QueryType.ErrorsTimeseries,
};

/** Non-secret settings (jsonData). */
export interface VakaOptions extends DataSourceJsonData {
  url?: string;
  defaultProject?: string;
  tlsSkipVerify?: boolean;
  tlsAuthWithCACert?: boolean;
}

/** Secrets (secureJsonData); only the backend can read them. */
export interface VakaSecureJsonData {
  token?: string;
  tlsCACert?: string;
}

export interface Option {
  text: string;
  value: string;
}

import {
  CoreApp,
  DataQueryRequest,
  DataQueryResponse,
  DataSourceInstanceSettings,
  LiveChannelScope,
  ScopedVars,
  StreamingFrameAction,
} from '@grafana/data';
import { DataSourceWithBackend, getGrafanaLiveSrv, getTemplateSrv } from '@grafana/runtime';
import { Observable, merge } from 'rxjs';

import { DEFAULT_QUERY, Option, QueryType, VakaOptions, VakaQuery } from './types';
import { VakaVariableSupport } from './variables';

const STRING_FIELDS: Array<keyof VakaQuery> = [
  'project',
  'groupBy',
  'bucket',
  'dimension',
  'eventType',
  'window',
  'issue',
  'level',
  'release',
  'environment',
  'exceptionType',
  'handled',
  'status',
  'sort',
  'search',
  'op',
  'transaction',
  'severity',
  'service',
  'traceId',
];

export class DataSource extends DataSourceWithBackend<VakaQuery, VakaOptions> {
  defaultProject: string;

  constructor(instanceSettings: DataSourceInstanceSettings<VakaOptions>) {
    super(instanceSettings);
    this.defaultProject = instanceSettings.jsonData.defaultProject ?? '';
    this.variables = new VakaVariableSupport(this);
    // Annotations use the regular query editor with the annotation query types.
    this.annotations = {};
  }

  getDefaultQuery(_: CoreApp): Partial<VakaQuery> {
    return { ...DEFAULT_QUERY, project: this.defaultProject || undefined };
  }

  applyTemplateVariables(query: VakaQuery, scopedVars: ScopedVars): VakaQuery {
    const srv = getTemplateSrv();
    const out: VakaQuery = { ...query };
    for (const key of STRING_FIELDS) {
      const v = out[key];
      if (typeof v === 'string' && v !== '') {
        (out as unknown as Record<string, unknown>)[key] = srv.replace(v, scopedVars);
      }
    }
    out.filters = query.filters?.map((f) => ({ ...f, value: srv.replace(f.value, scopedVars) }));
    out.steps = query.steps?.map((s) => srv.replace(s, scopedVars));
    out.metrics = query.metrics?.map((m) => srv.replace(m, scopedVars));
    return out;
  }

  filterQuery(query: VakaQuery): boolean {
    return !query.hide;
  }

  query(request: DataQueryRequest<VakaQuery>): Observable<DataQueryResponse> {
    const live = request.targets.filter((t) => t.queryType === QueryType.Live && !t.hide);
    if (live.length === 0) {
      return super.query(request);
    }
    const others = request.targets.filter((t) => t.queryType !== QueryType.Live);
    const streams = live.map((t) => {
      const q = this.applyTemplateVariables(t, request.scopedVars);
      const project = q.project || this.defaultProject;
      const parts = [q.stream || 'errors', project];
      if (q.environment) {
        parts.push(q.environment);
      }
      return getGrafanaLiveSrv().getDataStream({
        key: `${request.requestId}-${t.refId}`,
        // namespace is the older name of stream; set both for Grafana 12.
        // eslint-disable-next-line @typescript-eslint/no-deprecated
        addr: { scope: LiveChannelScope.DataSource, stream: this.uid, namespace: this.uid, path: parts.join('/') },
        buffer: { maxLength: t.limit && t.limit > 0 ? t.limit : 500, action: StreamingFrameAction.Append },
      });
    });
    if (others.length === 0) {
      return merge(...streams);
    }
    return merge(super.query({ ...request, targets: others }), ...streams);
  }

  /** Options for the editor dropdowns, served by the backend. */
  async options(path: string, params: Record<string, string | undefined> = {}): Promise<Option[]> {
    const clean: Record<string, string> = {};
    for (const [k, v] of Object.entries(params)) {
      if (v) {
        clean[k] = getTemplateSrv().replace(v);
      }
    }
    try {
      const res = await this.getResource<Option[]>(path, clean);
      return Array.isArray(res) ? res : [];
    } catch {
      return [];
    }
  }
}

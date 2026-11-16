import { CustomVariableSupport, DataQueryRequest, DataQueryResponse } from '@grafana/data';
import { Observable } from 'rxjs';

import type { DataSource } from './datasource';
import { VariableQueryEditor } from './components/VariableQueryEditor';
import { QueryType, VakaQuery } from './types';

/** Template variables (projects, releases, event names, dimension values) run as backend queries. */
export class VakaVariableSupport extends CustomVariableSupport<DataSource, VakaQuery> {
  editor = VariableQueryEditor;

  constructor(private readonly datasource: DataSource) {
    super();
  }

  query(request: DataQueryRequest<VakaQuery>): Observable<DataQueryResponse> {
    const targets = request.targets.map((t) => ({ ...t, queryType: t.queryType ?? QueryType.VarProjects }));
    return this.datasource.query({ ...request, targets });
  }
}

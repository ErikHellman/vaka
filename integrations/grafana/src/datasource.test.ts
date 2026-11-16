import { DataSourceInstanceSettings } from '@grafana/data';

import { DataSource } from './datasource';
import { QueryType, VakaOptions } from './types';

jest.mock('@grafana/runtime', () => ({
  DataSourceWithBackend: class {
    uid: string;
    constructor(s: { uid: string }) {
      this.uid = s.uid;
    }
  },
  getGrafanaLiveSrv: () => ({}),
  getTemplateSrv: () => ({
    replace: (s: string) => s.replace('$project', 'web').replace('$release', '1.2.0'),
    getVariables: () => [],
  }),
}));

const settings = {
  id: 1,
  uid: 'vaka',
  type: 'hellsoft-vaka-datasource',
  name: 'Vaka',
  jsonData: { defaultProject: 'app' },
} as unknown as DataSourceInstanceSettings<VakaOptions>;

describe('DataSource', () => {
  it('interpolates variables in strings, filters and steps', () => {
    const ds = new DataSource(settings);
    const q = ds.applyTemplateVariables(
      {
        refId: 'A',
        queryType: QueryType.ErrorsTimeseries,
        project: '$project',
        release: '$release',
        filters: [{ key: 'release', op: '=', value: '$release' }],
        steps: ['signup|release:$release'],
      },
      {}
    );
    expect(q.project).toBe('web');
    expect(q.release).toBe('1.2.0');
    expect(q.filters?.[0].value).toBe('1.2.0');
    expect(q.steps?.[0]).toBe('signup|release:1.2.0');
  });

  it('starts new queries on the default project', () => {
    const ds = new DataSource(settings);
    expect(ds.getDefaultQuery('explore' as never).project).toBe('app');
  });
});

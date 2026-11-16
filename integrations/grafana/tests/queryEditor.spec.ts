import { test, expect } from '@grafana/plugin-e2e';

test('smoke: should render query editor', async ({ panelEditPage, readProvisionedDataSource }) => {
  const ds = await readProvisionedDataSource({ fileName: 'datasources.yml' });
  await panelEditPage.datasource.set(ds.name);
  await expect(panelEditPage.getQueryEditorRow('A').getByTestId('vaka-query-type')).toBeVisible();
});

test('errors over time returns data from Vaka', async ({ panelEditPage, readProvisionedDataSource }) => {
  const ds = await readProvisionedDataSource({ fileName: 'datasources.yml' });
  await panelEditPage.datasource.set(ds.name);
  const res = await panelEditPage.refreshPanel();
  await expect(res).toBeOK();
  const body = await res.json();
  const frame = body.results.A.frames[0];
  // time + errors, with one row per bucket from the mock server.
  expect(frame.schema.fields.map((f: { name: string }) => f.name)).toEqual(['time', 'errors']);
  expect(frame.data.values[0].length).toBeGreaterThan(10);
});

import { test, expect } from '@grafana/plugin-e2e';
import { VakaOptions, VakaSecureJsonData } from '../src/types';

test('smoke: should render config editor', async ({ createDataSourceConfigPage, readProvisionedDataSource, page }) => {
  const ds = await readProvisionedDataSource({ fileName: 'datasources.yml' });
  await createDataSourceConfigPage({ type: ds.type });
  await expect(page.getByRole('textbox', { name: /Vaka URL/ })).toBeVisible();
});

test('"Save & test" should be successful when configuration is valid', async ({
  createDataSourceConfigPage,
  readProvisionedDataSource,
  page,
}) => {
  const ds = await readProvisionedDataSource<VakaOptions, VakaSecureJsonData>({ fileName: 'datasources.yml' });
  const configPage = await createDataSourceConfigPage({ type: ds.type });
  await page.getByRole('textbox', { name: /Vaka URL/ }).fill(ds.jsonData.url ?? '');
  await page.getByRole('textbox', { name: /API token/ }).fill(ds.secureJsonData?.token ?? '');
  await expect(configPage.saveAndTest()).toBeOK();
  await expect(configPage).toHaveAlert('success', { hasText: 'Connected to Vaka' });
});

test('"Save & test" should fail when the token is missing', async ({
  createDataSourceConfigPage,
  readProvisionedDataSource,
  page,
}) => {
  const ds = await readProvisionedDataSource<VakaOptions, VakaSecureJsonData>({ fileName: 'datasources.yml' });
  const configPage = await createDataSourceConfigPage({ type: ds.type });
  await page.getByRole('textbox', { name: /Vaka URL/ }).fill(ds.jsonData.url ?? '');
  await expect(configPage.saveAndTest()).not.toBeOK();
  await expect(configPage).toHaveAlert('error', { hasText: 'API token' });
});

test('"Save & test" should fail when the token is wrong', async ({
  createDataSourceConfigPage,
  readProvisionedDataSource,
  page,
}) => {
  const ds = await readProvisionedDataSource<VakaOptions, VakaSecureJsonData>({ fileName: 'datasources.yml' });
  const configPage = await createDataSourceConfigPage({ type: ds.type });
  await page.getByRole('textbox', { name: /Vaka URL/ }).fill(ds.jsonData.url ?? '');
  await page.getByRole('textbox', { name: /API token/ }).fill('vt_wrong');
  await expect(configPage.saveAndTest()).not.toBeOK();
  await expect(configPage).toHaveAlert('error', { hasText: 'rejected the API token' });
});

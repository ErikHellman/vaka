import React, { ChangeEvent } from 'react';
import { Field, FieldSet, InlineSwitch, Input, SecretInput, SecretTextArea, TextLink } from '@grafana/ui';
import { DataSourcePluginOptionsEditorProps } from '@grafana/data';
import { VakaOptions, VakaSecureJsonData } from '../types';

interface Props extends DataSourcePluginOptionsEditorProps<VakaOptions, VakaSecureJsonData> {}

export function ConfigEditor(props: Props) {
  const { onOptionsChange, options } = props;
  const { jsonData, secureJsonFields, secureJsonData } = options;

  const setJson = (patch: Partial<VakaOptions>) => onOptionsChange({ ...options, jsonData: { ...jsonData, ...patch } });
  const setSecret = (key: keyof VakaSecureJsonData, value: string) =>
    onOptionsChange({ ...options, secureJsonData: { ...secureJsonData, [key]: value } });
  const resetSecret = (key: keyof VakaSecureJsonData) =>
    onOptionsChange({
      ...options,
      secureJsonFields: { ...secureJsonFields, [key]: false },
      secureJsonData: { ...secureJsonData, [key]: '' },
    });

  return (
    <>
      <FieldSet label="Connection">
        <Field label="Vaka URL" description="The address of your Vaka server, without /api/v1." required>
          <Input
            id="config-editor-url"
            value={jsonData.url ?? ''}
            placeholder="https://vaka.example.com"
            width={50}
            onChange={(e: ChangeEvent<HTMLInputElement>) => setJson({ url: e.target.value })}
          />
        </Field>
        <Field
          label="API token"
          description={
            <>
              A read token, for all projects or one. Create it in the Vaka admin UI under Tokens. It stays on the
              Grafana server.
            </>
          }
          required
        >
          <SecretInput
            id="config-editor-token"
            isConfigured={!!secureJsonFields?.token}
            value={secureJsonData?.token ?? ''}
            placeholder="vt_..."
            width={50}
            onReset={() => resetSecret('token')}
            onChange={(e: ChangeEvent<HTMLInputElement>) => setSecret('token', e.target.value)}
          />
        </Field>
        <Field label="Default project" description="Slug of the project new queries start with (optional).">
          <Input
            id="config-editor-default-project"
            value={jsonData.defaultProject ?? ''}
            placeholder="web"
            width={30}
            onChange={(e: ChangeEvent<HTMLInputElement>) => setJson({ defaultProject: e.target.value })}
          />
        </Field>
      </FieldSet>
      <FieldSet label="TLS">
        <Field label="Skip TLS certificate verification" description="Only for testing against self-signed servers.">
          <InlineSwitch
            id="config-editor-tls-skip"
            value={!!jsonData.tlsSkipVerify}
            onChange={(e) => setJson({ tlsSkipVerify: e.currentTarget.checked })}
          />
        </Field>
        <Field label="Custom CA certificate" description="Trust a private certificate authority.">
          <InlineSwitch
            id="config-editor-tls-ca"
            value={!!jsonData.tlsAuthWithCACert}
            onChange={(e) => setJson({ tlsAuthWithCACert: e.currentTarget.checked })}
          />
        </Field>
        {jsonData.tlsAuthWithCACert && (
          <Field label="CA certificate (PEM)">
            <SecretTextArea
              isConfigured={!!secureJsonFields?.tlsCACert}
              value={secureJsonData?.tlsCACert ?? ''}
              cols={60}
              rows={6}
              placeholder="-----BEGIN CERTIFICATE-----"
              onReset={() => resetSecret('tlsCACert')}
              onChange={(e: ChangeEvent<HTMLTextAreaElement>) => setSecret('tlsCACert', e.target.value)}
            />
          </Field>
        )}
      </FieldSet>
      <TextLink href="https://vaka.hellsoft.se/grafana.html" external>
        Vaka Grafana documentation
      </TextLink>
    </>
  );
}

// Package models holds the data source configuration.
package models

import (
	"encoding/json"
	"fmt"
	"strings"

	"github.com/grafana/grafana-plugin-sdk-go/backend"
)

// PluginSettings is the non-secret configuration (jsonData).
type PluginSettings struct {
	// URL is the Vaka server, e.g. https://vaka.example.com. /api/v1 is appended.
	URL string `json:"url"`
	// DefaultProject is preselected in new queries (slug or id).
	DefaultProject string                `json:"defaultProject"`
	Secrets        *SecretPluginSettings `json:"-"`
}

// SecretPluginSettings is decrypted secureJsonData; never sent to the browser.
type SecretPluginSettings struct {
	Token string `json:"token"`
}

// LoadPluginSettings reads and validates the settings of a data source instance.
func LoadPluginSettings(source backend.DataSourceInstanceSettings) (*PluginSettings, error) {
	settings := PluginSettings{}
	if len(source.JSONData) > 0 {
		if err := json.Unmarshal(source.JSONData, &settings); err != nil {
			return nil, fmt.Errorf("could not unmarshal PluginSettings json: %w", err)
		}
	}
	if settings.URL == "" {
		settings.URL = source.URL
	}
	settings.URL = strings.TrimRight(strings.TrimSpace(settings.URL), "/")
	settings.URL = strings.TrimSuffix(settings.URL, "/api/v1")
	settings.Secrets = &SecretPluginSettings{Token: strings.TrimSpace(source.DecryptedSecureJSONData["token"])}
	return &settings, nil
}

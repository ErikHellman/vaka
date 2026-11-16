// Package plugin implements the Vaka data source backend.
package plugin

import (
	"context"
	"encoding/json"
	"fmt"
	"net/http"
	"net/url"
	"strings"
	"time"

	"github.com/ErikHellman/vaka/integrations/grafana/pkg/models"
	"github.com/ErikHellman/vaka/integrations/grafana/pkg/vaka"
	"github.com/grafana/grafana-plugin-sdk-go/backend"
	"github.com/grafana/grafana-plugin-sdk-go/backend/httpclient"
	"github.com/grafana/grafana-plugin-sdk-go/backend/instancemgmt"
	"github.com/grafana/grafana-plugin-sdk-go/backend/log"
	"github.com/grafana/grafana-plugin-sdk-go/data"
)

var (
	_ backend.QueryDataHandler      = (*Datasource)(nil)
	_ backend.CheckHealthHandler    = (*Datasource)(nil)
	_ backend.CallResourceHandler   = (*Datasource)(nil)
	_ backend.StreamHandler         = (*Datasource)(nil)
	_ instancemgmt.InstanceDisposer = (*Datasource)(nil)
)

// Version is set at build time; shown in the User-Agent.
var Version = "dev"

// Datasource is one configured Vaka data source.
type Datasource struct {
	settings *models.PluginSettings
	client   *vaka.Client
	// stream has no overall request timeout, for the SSE connection.
	stream *vaka.Client
}

// NewDatasource creates a data source instance from its settings.
func NewDatasource(ctx context.Context, s backend.DataSourceInstanceSettings) (instancemgmt.Instance, error) {
	settings, err := models.LoadPluginSettings(s)
	if err != nil {
		return nil, err
	}
	opts, err := s.HTTPClientOptions(ctx)
	if err != nil {
		return nil, fmt.Errorf("http client options: %w", err)
	}
	if opts.Timeouts.Timeout == 0 {
		opts.Timeouts.Timeout = 60 * time.Second
	}
	hc, err := httpclient.New(opts)
	if err != nil {
		return nil, fmt.Errorf("http client: %w", err)
	}
	streamOpts := opts
	streamOpts.Timeouts.Timeout = 0
	sc, err := httpclient.New(streamOpts)
	if err != nil {
		return nil, fmt.Errorf("stream http client: %w", err)
	}
	ua := "grafana-vaka-datasource/" + Version
	return &Datasource{
		settings: settings,
		client:   &vaka.Client{BaseURL: settings.URL, Token: settings.Secrets.Token, HTTP: hc, UserAgent: ua},
		stream:   &vaka.Client{BaseURL: settings.URL, Token: settings.Secrets.Token, HTTP: sc, UserAgent: ua},
	}, nil
}

// Dispose releases the instance when its settings change.
func (d *Datasource) Dispose() {
	d.client.HTTP.CloseIdleConnections()
	d.stream.HTTP.CloseIdleConnections()
}

// QueryData runs each query; one failing query does not fail the others.
func (d *Datasource) QueryData(ctx context.Context, req *backend.QueryDataRequest) (*backend.QueryDataResponse, error) {
	response := backend.NewQueryDataResponse()
	for _, q := range req.Queries {
		response.Responses[q.RefID] = d.query(ctx, q)
	}
	return response, nil
}

type handler func(*Datasource, context.Context, Query, backend.DataQuery) (data.Frames, error)

var handlers = map[string]handler{
	QTAnalyticsTimeseries: (*Datasource).analyticsTimeseries,
	QTAnalyticsBreakdown:  (*Datasource).analyticsBreakdown,
	QTAnalyticsSummary:    (*Datasource).analyticsSummary,
	QTRealtime:            (*Datasource).realtime,
	QTErrorsTimeseries:    (*Datasource).errorsTimeseries,
	QTIssues:              (*Datasource).issues,
	QTSessionsTimeseries:  (*Datasource).sessionsTimeseries,
	QTReleaseHealth:       (*Datasource).releaseHealth,
	QTReleaseAdoption:     (*Datasource).releaseAdoption,
	QTPerfTimeseries:      (*Datasource).perfTimeseries,
	QTPerfSummary:         (*Datasource).perfSummary,
	QTWebVitals:           (*Datasource).webVitals,
	QTLogs:                (*Datasource).logs,
	QTTrace:               (*Datasource).trace,
	QTFunnel:              (*Datasource).funnel,
	QTMonitors:            (*Datasource).monitors,
	QTIncidents:           (*Datasource).incidents,
	QTVarProjects:         (*Datasource).variableQuery,
	QTVarReleases:         (*Datasource).variableQuery,
	QTVarEventNames:       (*Datasource).variableQuery,
	QTVarDimensionValues:  (*Datasource).variableQuery,
	QTAnnReleases:         (*Datasource).annotations,
	QTAnnIncidents:        (*Datasource).annotations,
	QTAnnIssues:           (*Datasource).annotations,
}

// needsProject lists query types that run without a project.
var noProject = map[string]bool{QTVarProjects: true}

func (d *Datasource) query(ctx context.Context, dq backend.DataQuery) backend.DataResponse {
	var q Query
	if err := json.Unmarshal(dq.JSON, &q); err != nil {
		return backend.ErrDataResponse(backend.StatusBadRequest, fmt.Sprintf("invalid query: %v", err))
	}
	if q.QueryType == "" {
		q.QueryType = dq.QueryType
	}
	if q.QueryType == "" {
		q.QueryType = QTAnalyticsTimeseries
	}
	if q.Project == "" {
		q.Project = d.settings.DefaultProject
	}
	if q.QueryType == QTLive {
		// Live queries subscribe to a Grafana Live channel from the browser (see RunStream);
		// run as a normal query (for example by alerting) they return nothing.
		return backend.DataResponse{Frames: data.Frames{data.NewFrame(dq.RefID)}}
	}
	h, ok := handlers[q.QueryType]
	if !ok {
		return backend.ErrDataResponse(backend.StatusBadRequest, fmt.Sprintf("unknown query type %q", q.QueryType))
	}
	if q.Project == "" && !noProject[q.QueryType] {
		return backend.ErrDataResponse(backend.StatusBadRequest, "choose a project")
	}
	frames, err := h(d, ctx, q, dq)
	if err != nil {
		status := backend.StatusBadRequest
		var apiErr *vaka.APIError
		if ok := asAPIError(err, &apiErr); ok {
			status = backend.Status(apiErr.Status)
			if vaka.IsNotFound(err) && apiErr.Message == "" && isSeries(q.QueryType) {
				err = fmt.Errorf("%w (this Vaka server may be too old for %s; upgrade to 0.5 or later)", err, q.QueryType)
			}
		} else if ctx.Err() != nil {
			status = backend.StatusTimeout
		}
		return backend.ErrDataResponse(status, err.Error())
	}
	return backend.DataResponse{Frames: frames}
}

func isSeries(qt string) bool {
	return qt == QTErrorsTimeseries || qt == QTPerfTimeseries || qt == QTSessionsTimeseries
}

func asAPIError(err error, target **vaka.APIError) bool {
	for e := err; e != nil; {
		if a, ok := e.(*vaka.APIError); ok {
			*target = a
			return true
		}
		u, ok := e.(interface{ Unwrap() error })
		if !ok {
			return false
		}
		e = u.Unwrap()
	}
	return false
}

// me is GET /api/v1/me.
type me struct {
	Type          string   `json:"type"`
	Name          string   `json:"name"`
	Email         string   `json:"email"`
	Role          string   `json:"role"`
	Scope         string   `json:"scope"`
	ProjectID     *string  `json:"project_id"`
	ServerVersion string   `json:"server_version"`
	Capabilities  []string `json:"capabilities"`
}

// CheckHealth backs the "Save & test" button.
func (d *Datasource) CheckHealth(ctx context.Context, _ *backend.CheckHealthRequest) (*backend.CheckHealthResult, error) {
	fail := func(msg string) (*backend.CheckHealthResult, error) {
		return &backend.CheckHealthResult{Status: backend.HealthStatusError, Message: msg}, nil
	}
	if d.settings.URL == "" {
		return fail("Enter the URL of your Vaka server")
	}
	if u, err := url.Parse(d.settings.URL); err != nil || (u.Scheme != "http" && u.Scheme != "https") || u.Host == "" {
		return fail("The Vaka URL must look like https://vaka.example.com")
	}
	if d.settings.Secrets.Token == "" {
		return fail("Enter a Vaka API token (create a read token in Vaka under Tokens)")
	}
	var who me
	if err := d.client.Get(ctx, "me", nil, &who); err != nil {
		return fail(err.Error())
	}
	var projects struct {
		Projects []any `json:"projects"`
	}
	if err := d.client.Get(ctx, "projects", nil, &projects); err != nil {
		return fail(err.Error())
	}
	name := who.Name
	if who.Type == "user" {
		name = who.Email
	}
	msg := fmt.Sprintf("Connected to Vaka %s as %s %q, %d project(s) readable", orUnknown(who.ServerVersion), who.Type, name, len(projects.Projects))
	if who.Scope == "write" {
		msg += ". This token has write scope; Grafana only needs a read token"
	}
	if who.ServerVersion == "" || !contains(who.Capabilities, "errors_timeseries") {
		return &backend.CheckHealthResult{Status: backend.HealthStatusUnknown, Message: msg + ". This Vaka server is older than 0.5: errors, performance and session time series and live streaming are not available"}, nil
	}
	return &backend.CheckHealthResult{Status: backend.HealthStatusOk, Message: msg}, nil
}

func orUnknown(s string) string {
	if s == "" {
		return "(unknown version)"
	}
	return s
}

func contains(list []string, s string) bool {
	for _, x := range list {
		if x == s {
			return true
		}
	}
	return false
}

// CallResource serves the query editor's dropdowns, so the token never reaches the browser.
//
//	GET projects
//	GET releases?project=
//	GET event-names?project=
//	GET prop-keys?project=
//	GET dimension-values?project=&dimension=
//	GET me
func (d *Datasource) CallResource(ctx context.Context, req *backend.CallResourceRequest, sender backend.CallResourceResponseSender) error {
	u, err := url.Parse(req.URL)
	if err != nil {
		return sendJSON(sender, http.StatusBadRequest, map[string]string{"error": "bad url"})
	}
	q := u.Query()
	project := q.Get("project")
	if project == "" {
		project = d.settings.DefaultProject
	}
	var out any
	switch strings.Trim(req.Path, "/") {
	case "projects":
		out, err = d.projectOptions(ctx)
	case "releases":
		out, err = d.releaseOptions(ctx, project)
	case "event-names":
		out, err = d.eventNameOptions(ctx, project, "")
	case "prop-keys":
		out, err = d.propKeyOptions(ctx, project)
	case "dimension-values":
		to := time.Now()
		out, err = d.dimensionValueOptions(ctx, project, q.Get("dimension"), to.Add(-30*24*time.Hour), to)
	case "me":
		var who me
		err = d.client.Get(ctx, "me", nil, &who)
		out = who
	default:
		return sendJSON(sender, http.StatusNotFound, map[string]string{"error": "unknown resource " + req.Path})
	}
	if err != nil {
		status := http.StatusBadGateway
		var apiErr *vaka.APIError
		if asAPIError(err, &apiErr) {
			status = apiErr.Status
		}
		log.DefaultLogger.Debug("resource call failed", "path", req.Path, "error", err)
		return sendJSON(sender, status, map[string]string{"error": err.Error()})
	}
	if out == nil {
		out = []Option{}
	}
	return sendJSON(sender, http.StatusOK, out)
}

func sendJSON(sender backend.CallResourceResponseSender, status int, v any) error {
	body, err := json.Marshal(v)
	if err != nil {
		return err
	}
	return sender.Send(&backend.CallResourceResponse{Status: status, Headers: map[string][]string{"Content-Type": {"application/json"}}, Body: body})
}

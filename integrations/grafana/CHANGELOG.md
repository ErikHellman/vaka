# Changelog

## 0.1.0 (2026-11-17)

First version of the Vaka data source.

- Query types: errors over time, issues, sessions and crash-free rate, release health, release
  adoption, analytics over time, breakdown, summary, realtime visitors, funnels, performance over
  time, performance summary, Web Vitals, logs, traces, monitors and incidents.
- Alerting on every time series query type.
- Live streams of new errors, analytics events and transactions (Grafana Live).
- Template variables: projects, releases, event names and dimension values.
- Annotations: releases, new issues and regressions, monitor incidents.
- The API token stays on the Grafana server; editor dropdowns go through the backend.

# Vaka Go SDK

Errors and panics, breadcrumbs, release health, analytics events and feature flags for Go
services, on top of the standard OpenTelemetry SDK (traces and logs go to Vaka over OTLP/HTTP).

```sh
go get github.com/ErikHellman/vaka/sdks/go@latest
```

```go
client, err := vaka.Init(vaka.Config{Key: "vk_...", Endpoint: "https://vaka.example.com", Release: "1.4.0"})
if err != nil {
	log.Fatal(err)
}
defer client.Shutdown(context.Background())
defer vaka.Recover()

http.ListenAndServe(":8080", vaka.Middleware(mux))
```

- Reference: [docs/sdk-go.md](../../docs/sdk-go.md)
- Install guide for coding agents: [docs/install/go.md](../../docs/install/go.md)
- Releasing: [docs/releasing/go.md](../../docs/releasing/go.md)
- Example: [examples/demo](examples/demo)

Development: `go vet ./... && go test -race ./...` (Go 1.24+).

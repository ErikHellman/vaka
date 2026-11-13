// Command demo sends an error, a handled panic in an HTTP handler, a span, logs and an
// analytics event to Vaka.
//
//	VAKA_KEY=vk_... VAKA_ENDPOINT=http://localhost:8080 go run ./examples/demo
package main

import (
	"context"
	"errors"
	"fmt"
	"log"
	"log/slog"
	"net/http"
	"net/http/httptest"
	"os"
	"time"

	vaka "github.com/ErikHellman/vaka/sdks/go"
)

func main() {
	client, err := vaka.Init(vaka.Config{Release: "0.1.0-demo", Debug: true})
	if err != nil {
		log.Fatal(err)
	}
	defer client.Shutdown(context.Background())
	defer vaka.Recover()

	logger := slog.New(vaka.SlogHandler(slog.NewTextHandler(os.Stderr, nil)))
	ctx := vaka.WithScope(context.Background())

	flags, err := vaka.EvaluateFlags(ctx, vaka.FlagRequest{DistinctID: "demo-user"})
	if err != nil {
		logger.WarnContext(ctx, "flags unavailable", "err", err)
	}
	ctx = vaka.WithFlags(ctx, flags)

	ctx, span := vaka.StartSpan(ctx, "demo job")
	logger.InfoContext(ctx, "job started", "items", 3)
	if err := charge(ctx); err != nil {
		vaka.CaptureError(ctx, err)
	}
	vaka.Track(ctx, "job_finished", map[string]any{"kind": "demo"})
	span.End()

	mux := http.NewServeMux()
	mux.HandleFunc("GET /carts/{id}", func(w http.ResponseWriter, r *http.Request) {
		logger.InfoContext(r.Context(), "loading cart", "id", r.PathValue("id"))
		panic("cart " + r.PathValue("id") + " is corrupt")
	})
	srv := httptest.NewServer(vaka.Middleware(mux))
	defer srv.Close()
	resp, err := http.Get(srv.URL + "/carts/42")
	if err == nil {
		resp.Body.Close()
		fmt.Println("handler panic answered", resp.StatusCode)
	}
	logger.ErrorContext(ctx, "nightly import failed", "err", errors.New("upstream timeout"))

	fctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	if err := vaka.Flush(fctx); err != nil {
		log.Printf("flush: %v", err)
	}
}

var errDeclined = errors.New("card declined")

func charge(ctx context.Context) error {
	_, span := vaka.StartSpan(ctx, "charge card")
	defer span.End()
	return fmt.Errorf("charge order 1001: %w", errDeclined)
}

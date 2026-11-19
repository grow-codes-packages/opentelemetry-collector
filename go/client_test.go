package otelcollector

import (
	"context"
	"errors"
	"net/http"
	"strings"
	"testing"
	"time"

	"google.golang.org/grpc/codes"
)

// newTestClient builds a client whose OTLP, health and zpages endpoints all point at
// url (fast retries); mutate adjusts the config.
func newTestClient(t *testing.T, base string, mutate func(*Config)) *Client {
	t.Helper()
	cfg := Config{
		CommonConfig: CommonConfig{URL: base, Retry: fastRetry(3)},
		HealthURL:    base + "/health",
		ZPagesURL:    base + "/zpages",
	}
	if mutate != nil {
		mutate(&cfg)
	}
	c, err := NewClient(cfg)
	if err != nil {
		t.Fatalf("NewClient: %v", err)
	}
	t.Cleanup(func() { _ = c.Close() })
	return c
}

// collectorMock serves recorded fixtures on the paths of newTestClient.
func collectorMock(t *testing.T) *testServer {
	health := loadFixture(t, "health.ok")
	zpages := loadFixture(t, "serverVersion.zpages")
	ping := loadFixture(t, "ping.http_empty_export")
	unauth := loadFixture(t, "ping.http_unauthenticated_missing_token")
	return newTestServer(t, func(w http.ResponseWriter, r *http.Request, _ int) {
		switch r.URL.Path {
		case "/health":
			health.serve(w)
		case "/zpages/debug/servicez":
			zpages.serve(w)
		case "/v1/traces":
			if r.Method != http.MethodPost || r.Header.Get("Content-Type") != "application/x-protobuf" {
				w.WriteHeader(http.StatusUnsupportedMediaType)
				return
			}
			if r.Header.Get("Authorization") == "Bearer "+testToken {
				ping.serve(w)
				return
			}
			unauth.serve(w)
		default:
			w.WriteHeader(http.StatusNotFound)
		}
	})
}

func authCfg(c *Config) { c.Headers = map[string]string{"Authorization": "Bearer " + testToken} }

func TestClientCore(t *testing.T) {
	ts := collectorMock(t)

	t.Run("health ping version", func(t *testing.T) {
		c := newTestClient(t, ts.URL, authCfg)
		hs, err := c.Health(t.Context())
		if err != nil || !hs.Healthy || hs.Status != "ok" || hs.Details["vendorStatus"] != "Server available" ||
			hs.Details["upSince"] == "" || hs.Details["source"] != "health_check" {
			t.Fatalf("health %+v %v", hs, err)
		}
		if d, err := c.Ping(t.Context()); err != nil || d <= 0 {
			t.Fatalf("ping %s %v", d, err)
		}
		if v, err := c.ServerVersion(t.Context()); err != nil || v != "0.165.0" {
			t.Fatalf("version %q %v", v, err)
		}
		if c.Config().URL != ts.URL {
			t.Fatal("Config()")
		}
		if ua := ts.request(0).Header.Get("User-Agent"); ua != UserAgent {
			t.Fatalf("core.useragent: %q", ua)
		}
	})

	t.Run("ping authentication error carries the rpc status message", func(t *testing.T) {
		c := newTestClient(t, ts.URL, nil)
		_, err := c.Ping(t.Context())
		if !errors.Is(err, ErrAuthentication) || !strings.Contains(err.Error(), "missing or empty authorization header") ||
			!strings.Contains(err.Error(), "(HTTP 401)") {
			t.Fatalf("ping: %v", err)
		}
		bad := newTestClient(t, ts.URL, func(cfg *Config) { cfg.Headers = map[string]string{"Authorization": "Bearer wrong-SECRET"} })
		_, err = bad.Ping(t.Context())
		if !errors.Is(err, ErrAuthentication) || strings.Contains(err.Error(), "wrong-SECRET") {
			t.Fatalf("wrong token: %v", err)
		}
	})

	t.Run("ping maps other statuses", func(t *testing.T) {
		for name, want := range map[string]error{
			"export.http_not_found":              ErrNotFound,
			"export.http_method_not_allowed":     ErrValidation,
			"export.http_unsupported_media_type": ErrValidation,
			"export.http_bad_request_protobuf":   ErrValidation,
			"export.http_bad_request_json":       ErrValidation,
		} {
			srv := newTestServer(t, serveFixture(t, name))
			c := newTestClient(t, srv.URL, nil)
			_, err := c.Ping(t.Context())
			if !errors.Is(err, want) {
				t.Fatalf("%s: %v", name, err)
			}
		}
		srv := newTestServer(t, func(w http.ResponseWriter, _ *http.Request, _ int) {
			w.Header().Set("Retry-After", "7")
			w.WriteHeader(http.StatusTooManyRequests)
		})
		_, err := newTestClient(t, srv.URL, nil).Ping(t.Context())
		var e *Error
		if !errors.As(err, &e) || e.Code != ErrorCodeRateLimited || e.RetryAfter != 7*time.Second {
			t.Fatalf("429: %v", err)
		}
	})

	t.Run("traces url is used as is", func(t *testing.T) {
		srv := newTestServer(t, func(w http.ResponseWriter, r *http.Request, _ int) {
			if r.URL.Path != "/custom/path" {
				w.WriteHeader(http.StatusNotFound)
			}
		})
		c := newTestClient(t, srv.URL, func(cfg *Config) { cfg.TracesURL = srv.URL + "/custom/path" })
		if _, err := c.Ping(t.Context()); err != nil {
			t.Fatal(err)
		}
	})

	t.Run("server version fallbacks", func(t *testing.T) {
		stock := newTestServer(t, func(w http.ResponseWriter, r *http.Request, _ int) {
			if r.URL.Path == "/debug/servicez" {
				loadFixture(t, "serverVersion.zpages_stock").serve(w)
				return
			}
			w.WriteHeader(http.StatusNotFound)
		})
		c := newTestClient(t, stock.URL, func(cfg *Config) { cfg.ZPagesURL = stock.URL })
		if v, err := c.ServerVersion(t.Context()); err != nil || v != "0.165.0" {
			t.Fatalf("stock zpages: %q %v", v, err)
		}

		metrics := newTestServer(t, func(w http.ResponseWriter, r *http.Request, _ int) {
			if r.URL.Path == "/metrics" {
				loadFixture(t, "serverVersion.telemetry_target_info").serve(w)
				return
			}
			w.WriteHeader(http.StatusNotFound)
		})
		c = newTestClient(t, metrics.URL, func(cfg *Config) { cfg.TelemetryURL = metrics.URL + "/metrics" })
		if v, err := c.ServerVersion(t.Context()); err != nil || v != "0.165.0" {
			t.Fatalf("telemetry fallback: %q %v", v, err)
		}

		tracez := newTestServer(t, serveFixture(t, "zpages.tracez"))
		c = newTestClient(t, tracez.URL, func(cfg *Config) { cfg.TelemetryURL = tracez.URL + "/metrics" })
		if _, err := c.ServerVersion(t.Context()); !errors.Is(err, ErrUnsupported) {
			t.Fatalf("no version: %v", err)
		}
		pipelinez := newTestServer(t, serveFixture(t, "zpages.pipelinez"))
		if _, err := newTestClient(t, pipelinez.URL, nil).ServerVersion(t.Context()); !errors.Is(err, ErrUnsupported) {
			t.Fatalf("page without version: %v", err)
		}
		if _, err := newTestClient(t, "http://"+closedAddr(t), func(cfg *Config) {
			cfg.Retry = fastRetry(0)
		}).ServerVersion(t.Context()); !errors.Is(err, ErrUnsupported) {
			t.Fatalf("unreachable: %v", err)
		}
		authSrv := newTestServer(t, statusHandler(http.StatusUnauthorized))
		if _, err := newTestClient(t, authSrv.URL, nil).ServerVersion(t.Context()); !errors.Is(err, ErrAuthentication) {
			t.Fatalf("auth: %v", err)
		}
		c = newTestClient(t, authSrv.URL, func(cfg *Config) { cfg.ZPagesURL = tracez.URL; cfg.TelemetryURL = authSrv.URL + "/m" })
		if _, err := c.ServerVersion(t.Context()); !errors.Is(err, ErrAuthentication) {
			t.Fatalf("telemetry auth: %v", err)
		}
		ctx, cancel := context.WithCancel(t.Context())
		cancel()
		if _, err := newTestClient(t, ts.URL, nil).ServerVersion(ctx); !errors.Is(err, ErrCancelled) {
			t.Fatalf("cancelled: %v", err)
		}
	})

	t.Run("core.close.idempotent", func(t *testing.T) {
		c := newTestClient(t, ts.URL, authCfg)
		calls := 0
		if err := c.register(func(context.Context) error { calls++; return nil }); err != nil {
			t.Fatal(err)
		}
		first, second := c.Close(), c.Close()
		if first != nil || second != nil || calls != 1 {
			t.Fatalf("close: %v %v %d", first, second, calls)
		}
		if _, err := c.Ping(t.Context()); !errors.Is(err, ErrCancelled) {
			t.Fatalf("ping after close: %v", err)
		}
		if _, err := c.ServerVersion(t.Context()); !errors.Is(err, ErrCancelled) {
			t.Fatalf("version after close: %v", err)
		}
		if _, err := c.Health(t.Context()); !errors.Is(err, ErrCancelled) {
			t.Fatalf("health after close: %v", err)
		}
		if err := c.register(func(context.Context) error { return nil }); !errors.Is(err, ErrCancelled) {
			t.Fatalf("register after close: %v", err)
		}
		if _, err := c.LogExporter(); !errors.Is(err, ErrCancelled) {
			t.Fatalf("exporter after close: %v", err)
		}
		if _, err := c.LogClient(LogClientOptions{}); !errors.Is(err, ErrCancelled) {
			t.Fatalf("log client after close: %v", err)
		}
	})

	t.Run("close reports closer errors", func(t *testing.T) {
		c := newTestClient(t, ts.URL, nil)
		_ = c.register(func(context.Context) error { return ValidationError("x", "boom") })
		if err := c.Close(); !errors.Is(err, ErrValidation) {
			t.Fatalf("close: %v", err)
		}
	})

	t.Run("query clients are unsupported", func(t *testing.T) {
		c := newTestClient(t, ts.URL, nil)
		ctx := t.Context()
		q := c.TraceQuery()
		checks := []error{}
		_, err := q.GetTrace(ctx, "4bf92f3577b34da6a3ce929d0e0e4736")
		checks = append(checks, err)
		_, err = q.SearchTraces(ctx, TraceSearchQuery{})
		checks = append(checks, err)
		_, err = q.ListServices(ctx)
		checks = append(checks, err)
		_, err = q.ListOperations(ctx, "svc")
		checks = append(checks, err)
		_, err = q.GetDependencies(ctx, DependencyOptions{})
		checks = append(checks, err)
		_, err = q.WaitForTrace(ctx, "4bf92f3577b34da6a3ce929d0e0e4736", WaitForTraceOptions{})
		checks = append(checks, err)
		lq := c.LogQuery()
		_, err = lq.Search(ctx, "x", SearchOptions{})
		checks = append(checks, err)
		_, err = lq.ListStreams(ctx)
		checks = append(checks, err)
		for i, err := range checks {
			if !errors.Is(err, ErrUnsupported) {
				t.Fatalf("check %d: %v", i, err)
			}
		}
	})

	t.Run("from env", func(t *testing.T) {
		t.Setenv(EnvPrefix+"URL", ts.URL)
		t.Setenv(EnvPrefix+"HEADERS", "Authorization=Bearer%20"+testToken)
		c, err := NewClientFromEnv()
		if err != nil {
			t.Fatal(err)
		}
		defer func() { _ = c.Close() }()
		if _, err := c.Ping(t.Context()); err != nil {
			t.Fatal(err)
		}
		t.Setenv(EnvPrefix+"URL", "mailto:x")
		if _, err := NewClientFromEnv(); !errors.Is(err, ErrConfig) {
			t.Fatalf("bad env: %v", err)
		}
	})

	t.Run("invalid config", func(t *testing.T) {
		for _, cfg := range []Config{
			{CommonConfig: CommonConfig{URL: "mailto:x"}},
			{CommonConfig: CommonConfig{TLS: TLSConfig{CACertPEM: "bad"}}},
			{CommonConfig: CommonConfig{URL: "http://h:4318", TLS: TLSConfig{CACertPEM: "bad"}}, Protocol: ProtocolGRPC},
		} {
			if _, err := NewClient(cfg); !errors.Is(err, ErrConfig) {
				t.Fatalf("NewClient(%s): %v", cfg, err)
			}
		}
		if _, err := newEndpoint(CommonConfig{}, "::bad", nil); !errors.Is(err, ErrConfig) {
			t.Fatalf("endpoint: %v", err)
		}
		if _, err := newEndpoint(CommonConfig{TLS: TLSConfig{CACertPEM: "bad"}}, "http://h", nil); !errors.Is(err, ErrConfig) {
			t.Fatalf("endpoint tls: %v", err)
		}
		if _, err := NewCollectorHealthClient(Config{Protocol: ProtocolNative}); !errors.Is(err, ErrConfig) {
			t.Fatalf("health client: %v", err)
		}
	})
}

func TestClientHealth(t *testing.T) {
	t.Run("503 is unhealthy", func(t *testing.T) {
		srv := newTestServer(t, func(w http.ResponseWriter, _ *http.Request, _ int) {
			w.Header().Set("Content-Type", "application/json")
			w.WriteHeader(http.StatusServiceUnavailable)
			_, _ = w.Write([]byte(`{"status":"Server not available","upSince":"0001-01-01T00:00:00Z","uptime":"0s"}`))
		})
		hs, err := newTestClient(t, srv.URL, nil).Health(t.Context())
		if err != nil || hs.Healthy || hs.Status != "unhealthy" || hs.Details["vendorStatus"] != "Server not available" || hs.Details["httpStatus"] != "503" {
			t.Fatalf("hs=%+v err=%v", hs, err)
		}
	})

	t.Run("zpages fallback on a stock stack", func(t *testing.T) {
		zp := newTestServer(t, serveFixture(t, "serverVersion.zpages_stock"))
		c := newTestClient(t, zp.URL, func(cfg *Config) {
			cfg.HealthURL = "http://" + closedAddr(t) + "/"
			cfg.ZPagesURL = zp.URL
			cfg.ConnectTimeout = time.Second
		})
		hs, err := c.Health(t.Context())
		if err != nil || !hs.Healthy || hs.Status != "ok" || hs.Details["source"] != "zpages" || hs.Details["healthCheckError"] == "" {
			t.Fatalf("hs=%+v err=%v", hs, err)
		}
	})

	t.Run("core.health.unreachable", func(t *testing.T) {
		addr := "http://" + closedAddr(t)
		c := newTestClient(t, addr, func(cfg *Config) { cfg.ConnectTimeout = time.Second })
		hs, err := c.Health(t.Context())
		if err != nil || hs.Healthy || hs.Status != "unreachable" {
			t.Fatalf("hs=%+v err=%v", hs, err)
		}
		zp := newTestServer(t, statusHandler(http.StatusNotFound))
		c = newTestClient(t, addr, func(cfg *Config) { cfg.ZPagesURL = zp.URL })
		if hs, err := c.Health(t.Context()); err != nil || hs.Status != "unreachable" {
			t.Fatalf("zpages 404: %+v %v", hs, err)
		}
	})

	t.Run("cancelled", func(t *testing.T) {
		srv := collectorMock(t)
		ctx, cancel := context.WithCancel(t.Context())
		cancel()
		if _, err := newTestClient(t, srv.URL, nil).Health(ctx); !errors.Is(err, ErrCancelled) {
			t.Fatalf("health: %v", err)
		}
		// cancelled during the zpages fallback
		zctx, zcancel := context.WithCancel(t.Context())
		zp := newTestServer(t, func(_ http.ResponseWriter, r *http.Request, _ int) { zcancel(); <-r.Context().Done() })
		c := newTestClient(t, zp.URL, func(cfg *Config) { cfg.HealthURL = "http://" + closedAddr(t) })
		if _, err := c.Health(zctx); !errors.Is(err, ErrCancelled) {
			t.Fatalf("fallback cancelled: %v", err)
		}
	})

	t.Run("wait until ready", func(t *testing.T) {
		ok := loadFixture(t, "health.ok")
		srv := newTestServer(t, func(w http.ResponseWriter, _ *http.Request, n int) {
			if n < 3 {
				w.WriteHeader(http.StatusServiceUnavailable)
				return
			}
			ok.serve(w)
		})
		c := newTestClient(t, srv.URL, nil)
		h := c.CollectorHealth()
		h.interval = time.Millisecond
		hs, err := h.WaitUntilReady(t.Context(), 5*time.Second)
		if err != nil || !hs.Healthy || srv.calls.Load() != 3 {
			t.Fatalf("ready: %+v %v calls=%d", hs, err, srv.calls.Load())
		}
		down := newTestServer(t, statusHandler(http.StatusServiceUnavailable))
		_, err = newTestClient(t, down.URL, nil).WaitUntilReady(t.Context(), 50*time.Millisecond)
		if !errors.Is(err, ErrTimeout) {
			t.Fatalf("timeout: %v", err)
		}
		ctx, cancel := context.WithCancel(t.Context())
		cancel()
		if _, err := newTestClient(t, down.URL, nil).WaitUntilReady(ctx, 0); !errors.Is(err, ErrCancelled) {
			t.Fatalf("cancel: %v", err)
		}
	})

	t.Run("standalone health client", func(t *testing.T) {
		srv := collectorMock(t)
		h, err := NewCollectorHealthClient(Config{CommonConfig: CommonConfig{URL: srv.URL}, HealthURL: srv.URL + "/health", ZPagesURL: srv.URL + "/zpages"})
		if err != nil {
			t.Fatal(err)
		}
		if hs, err := h.Health(t.Context()); err != nil || !hs.Healthy {
			t.Fatalf("%+v %v", hs, err)
		}
		if err := h.Close(); err != nil {
			t.Fatal(err)
		}
		if _, err := h.Health(t.Context()); !errors.Is(err, ErrCancelled) {
			t.Fatalf("after close: %v", err)
		}
		c := newTestClient(t, srv.URL, nil)
		if err := c.CollectorHealth().Close(); err != nil {
			t.Fatal(err)
		}
	})
}

func TestClientGRPC(t *testing.T) {
	srv := newOTLPServer(t)
	grpcCfg := func(cfg *Config) {
		cfg.Protocol = ProtocolGRPC
		cfg.GRPCURL = srv.url()
		cfg.Headers = map[string]string{"Authorization": "Bearer " + testToken}
	}
	c := newTestClient(t, "http://127.0.0.1:1", grpcCfg)
	if d, err := c.Ping(t.Context()); err != nil || d <= 0 {
		t.Fatalf("ping: %v", err)
	}
	if n, _, _, md := srv.snapshot(); n != 1 || md[0].Get("authorization")[0] != "Bearer "+testToken ||
		!strings.HasPrefix(md[0].Get("user-agent")[0], UserAgent) {
		t.Fatalf("server saw %d calls, md %v", n, md)
	}

	srv.mu.Lock()
	srv.fail = func(int) error {
		return grpcErr(codes.Unauthenticated, "provided authorization does not match expected scheme or token")
	}
	srv.mu.Unlock()
	_, err := c.Ping(t.Context())
	if !errors.Is(err, ErrAuthentication) || !strings.Contains(err.Error(), "(gRPC 16)") || strings.Contains(err.Error(), testToken) {
		t.Fatalf("unauthenticated: %v", err)
	}

	// UNAVAILABLE is retried with the retry policy.
	srv.mu.Lock()
	srv.calls = 0
	srv.fail = func(n int) error {
		if n < 3 {
			return grpcErr(codes.Unavailable, "busy")
		}
		return nil
	}
	srv.mu.Unlock()
	if _, err := c.Ping(t.Context()); err != nil {
		t.Fatalf("retried: %v", err)
	}
	srv.mu.Lock()
	calls := srv.calls
	srv.mu.Unlock()
	if calls != 3 {
		t.Fatalf("calls %d", calls)
	}

	ctx, cancel := context.WithCancel(t.Context())
	cancel()
	if _, err := c.Ping(ctx); !errors.Is(err, ErrCancelled) {
		t.Fatalf("cancelled: %v", err)
	}

	// Unreachable target, no retries.
	down := newTestClient(t, "http://127.0.0.1:1", func(cfg *Config) {
		cfg.Protocol = ProtocolGRPC
		cfg.GRPCURL = "http://" + closedAddr(t)
		cfg.Retry = fastRetry(1)
		cfg.Compression = CompressionNone
	})
	if _, err := down.Ping(t.Context()); !errors.Is(err, ErrUnavailable) {
		t.Fatalf("unreachable: %v", err)
	}
}

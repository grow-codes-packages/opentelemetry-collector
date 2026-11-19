package otelcollector

import (
	"context"
	"time"
)

// Default WaitUntilReady settings.
const (
	DefaultReadyTimeout  = 30 * time.Second
	DefaultReadyInterval = 500 * time.Millisecond
)

// CollectorHealthClient checks the collector's health_check extension (port 13133)
// with a zpages fallback. Get it from Client.CollectorHealth or
// NewCollectorHealthClient.
type CollectorHealthClient struct {
	health   *endpoint
	zpages   *endpoint
	closed   func() bool
	interval time.Duration
	own      *Client
}

// NewCollectorHealthClient returns a standalone health client for cfg (HealthURL and
// ZPagesURL). Close releases its connections.
func NewCollectorHealthClient(cfg Config) (*CollectorHealthClient, error) {
	c, err := NewClient(cfg)
	if err != nil {
		return nil, err
	}
	h := c.CollectorHealth()
	h.own = c
	return h, nil
}

// Close releases a standalone health client (no-op for one from Client.CollectorHealth).
func (h *CollectorHealthClient) Close() error {
	if h.own != nil {
		return h.own.Close()
	}
	return nil
}

// Health reports the collector health (see Client.Health). It fails only for
// CANCELLED and CONFIG.
func (h *CollectorHealthClient) Health(ctx context.Context) (HealthStatus, error) {
	if h.closed != nil && h.closed() {
		return HealthStatus{}, &Error{Code: ErrorCodeCancelled, Message: "health: client is closed"}
	}
	hs, err := h.health.t.checkHealth(ctx, "health", h.health.get("health", "", "application/json"))
	if err != nil {
		return HealthStatus{}, err
	}
	if hs.Status == "unreachable" {
		zs, zerr := h.zpages.t.checkHealth(ctx, "health", h.zpages.get("health", "/debug/servicez", "text/html"))
		if zerr != nil {
			return HealthStatus{}, zerr
		}
		if zs.Healthy {
			return HealthStatus{Healthy: true, Status: "ok", Details: map[string]string{
				"source":           "zpages",
				"httpStatus":       zs.Details["httpStatus"],
				"healthCheckError": hs.Details["error"],
			}}, nil
		}
		return hs, nil
	}
	hs.Details["source"] = "health_check"
	if hs.Status != "" && hs.Status != "ok" && hs.Status != "unhealthy" {
		hs.Details["vendorStatus"] = hs.Status
	}
	if hs.Healthy {
		hs.Status = "ok"
	} else {
		hs.Status = "unhealthy"
	}
	return hs, nil
}

// WaitUntilReady polls Health every 500 ms until the collector is healthy and returns
// the last status. TIMEOUT when it is not healthy within timeout (0 = 30 s);
// CANCELLED when ctx is cancelled.
func (h *CollectorHealthClient) WaitUntilReady(ctx context.Context, timeout time.Duration) (HealthStatus, error) {
	if timeout <= 0 {
		timeout = DefaultReadyTimeout
	}
	interval := h.interval
	if interval <= 0 {
		interval = DefaultReadyInterval
	}
	return PollUntil(ctx, "health.waitUntilReady", timeout, interval, func(ctx context.Context) (HealthStatus, bool, error) {
		hs, err := h.Health(ctx)
		if err != nil {
			return hs, false, err
		}
		return hs, hs.Healthy, nil
	})
}

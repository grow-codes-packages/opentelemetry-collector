package otelcollector

import (
	"context"
	"errors"
	"net/http"
	"strings"
	"testing"

	"google.golang.org/grpc/codes"
	"google.golang.org/protobuf/encoding/protowire"
)

func TestProtoDecoding(t *testing.T) {
	ps := decodePartialSuccess([]byte{0x0a, 0x05, 0x08, 0x02, 0x12, 0x01, 0x78})
	if ps.rejected != 2 || ps.message != "x" {
		t.Fatalf("partial: %+v", ps)
	}
	if p := decodePartialSuccess([]byte{0x0a, 0x00}); p.rejected != 0 || p.message != "" {
		t.Fatal("empty partial")
	}
	for _, bad := range [][]byte{{0xff}, {0x0a, 0x05, 0x08}, {0x08}, {0x0a, 0x02, 0xff, 0xff}} {
		if p := decodePartialSuccess(bad); p != (partial{}) {
			t.Fatalf("bad %x: %+v", bad, p)
		}
	}
	if p := decodePartialSuccess(nil); p != (partial{}) {
		t.Fatal("nil")
	}
	f := loadFixture(t, "export.http_bad_request_protobuf")
	code, msg, ok := decodeRPCStatus(f.body())
	if !ok || code != 3 || !strings.Contains(msg, "illegal wireType") {
		t.Fatalf("status: %d %q %v", code, msg, ok)
	}
	if _, _, ok := decodeRPCStatus(nil); ok {
		t.Fatal("empty status")
	}
	// fixed32 / repeated fields are skipped
	b := protowire.AppendTag(nil, 3, protowire.Fixed32Type)
	b = protowire.AppendFixed32(b, 7)
	b = protowire.AppendTag(b, 1, protowire.VarintType)
	b = protowire.AppendVarint(b, 5)
	b = protowire.AppendTag(b, 1, protowire.VarintType)
	b = protowire.AppendVarint(b, 6)
	b = protowire.AppendTag(b, 2, protowire.BytesType)
	b = protowire.AppendString(b, "first")
	b = protowire.AppendTag(b, 2, protowire.BytesType)
	b = protowire.AppendString(b, "second")
	if code, msg, ok := decodeRPCStatus(b); !ok || code != 5 || msg != "first" {
		t.Fatalf("skip: %d %q", code, msg)
	}
	bad := protowire.AppendTag(nil, 3, protowire.Fixed64Type)
	if _, _, ok := decodeRPCStatus(append(bad, 1)); ok {
		t.Fatal("truncated fixed64")
	}

	res := &response{Header: http.Header{"Content-Type": {"application/x-protobuf"}}, Body: []byte{0xff}}
	if otlpErrorText(res) != "" {
		t.Fatal("undecodable status")
	}
	res = &response{Header: http.Header{"Content-Type": {"text/plain"}}, Body: []byte("404 page not found\n")}
	if otlpErrorText(res) != "404 page not found" {
		t.Fatal("text")
	}
}

func TestGRPCError(t *testing.T) {
	cases := map[codes.Code]error{
		codes.Unauthenticated:    ErrAuthentication,
		codes.PermissionDenied:   ErrPermissionDenied,
		codes.NotFound:           ErrNotFound,
		codes.Unimplemented:      ErrNotFound,
		codes.AlreadyExists:      ErrConflict,
		codes.ResourceExhausted:  ErrRateLimited,
		codes.DeadlineExceeded:   ErrTimeout,
		codes.Canceled:           ErrCancelled,
		codes.Unavailable:        ErrUnavailable,
		codes.Aborted:            ErrUnavailable,
		codes.Unknown:            ErrUnavailable,
		codes.DataLoss:           ErrUnavailable,
		codes.Code(99):           ErrUnavailable,
		codes.OutOfRange:         ErrValidation,
		codes.InvalidArgument:    ErrValidation,
		codes.Internal:           ErrValidation,
		codes.FailedPrecondition: ErrValidation,
	}
	for c, want := range cases {
		err := grpcError(context.Background(), "op", grpcErr(c, "msg tok-SECRET"), []string{"tok-SECRET"})
		if !errors.Is(err, want) || strings.Contains(err.Error(), "tok-SECRET") {
			t.Fatalf("%s: %v", c, err)
		}
	}
	if err := grpcError(context.Background(), "op", grpcErr(codes.Internal, ""), nil); err.Message != "op: internal (gRPC 13)" {
		t.Fatalf("empty message: %q", err.Message)
	}
	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	if err := grpcError(ctx, "op", grpcErr(codes.Unavailable, "x"), nil); err.Code != ErrorCodeCancelled {
		t.Fatalf("ctx: %v", err)
	}
	if _, _, err := grpcHostPort("::bad"); !errors.Is(err, ErrConfig) {
		t.Fatal("bad target")
	}
	if hp, secure, err := grpcHostPort("https://h"); err != nil || hp != "h:4317" || !secure {
		t.Fatalf("default port: %s %v %v", hp, secure, err)
	}
}

func TestGRPCTargetConfig(t *testing.T) {
	if _, err := newGRPCTarget(Config{Protocol: ProtocolGRPC, GRPCURL: "https://h:4317", CommonConfig: CommonConfig{TLS: TLSConfig{CACertPEM: "bad"}}}); !errors.Is(err, ErrConfig) {
		t.Fatalf("tls: %v", err)
	}
	g, err := newGRPCTarget(Config{Protocol: ProtocolGRPC, GRPCURL: "https://h:4317", CommonConfig: CommonConfig{UserAgentSuffix: "app/1"}})
	if err != nil {
		t.Fatal(err)
	}
	_ = g.close()
	if _, err := newGRPCTarget(Config{Protocol: ProtocolGRPC, GRPCURL: "::bad"}); !errors.Is(err, ErrConfig) {
		t.Fatal("invalid url")
	}
}

package main

import (
	"net/http"
	"net/url"
	"testing"

	acl "github.com/stripe/smokescreen/pkg/smokescreen/acl/v1"
)

type allowAll struct{}

func (allowAll) Decide(acl.DecideArgs) (acl.Decision, error) {
	return acl.Decision{Result: acl.Allow, Reason: "inner"}, nil
}

func connectReq(authority string) *http.Request {
	return &http.Request{Method: http.MethodConnect, Host: authority, URL: &url.URL{Host: authority}}
}

func proxyReq(rawURL string) *http.Request {
	u, err := url.Parse(rawURL)
	if err != nil {
		panic(err)
	}
	return &http.Request{Method: http.MethodGet, Host: u.Host, URL: u}
}

func TestPortGuard(t *testing.T) {
	ports, err := parsePorts("")
	if err != nil {
		t.Fatal(err)
	}
	g := portGuard{inner: allowAll{}, allowed: ports}
	cases := []struct {
		name string
		req  *http.Request
		want acl.DecisionResult
	}{
		{"connect 443", connectReq("example.com:443"), acl.Allow},
		{"connect 80", connectReq("example.com:80"), acl.Allow},
		{"connect 8443", connectReq("example.com:8443"), acl.Deny},
		{"connect 22", connectReq("example.com:22"), acl.Deny},
		{"connect without port", connectReq("example.com"), acl.Deny},
		{"http default port", proxyReq("http://example.com/x"), acl.Allow},
		{"http explicit 80", proxyReq("http://example.com:80/x"), acl.Allow},
		{"http 8080", proxyReq("http://example.com:8080/x"), acl.Deny},
		{"https scheme default", proxyReq("https://example.com/x"), acl.Allow},
		{"ws scheme", proxyReq("ws://example.com/x"), acl.Deny},
		{"nil request", nil, acl.Deny},
	}
	for _, c := range cases {
		d, err := g.Decide(acl.DecideArgs{Req: c.req, Host: "example.com"})
		if err != nil {
			t.Fatalf("%s: %v", c.name, err)
		}
		if d.Result != c.want {
			t.Errorf("%s: got %v (%s), want %v", c.name, d.Result, d.Reason, c.want)
		}
	}
}

func TestPortGuardWithoutInnerACL(t *testing.T) {
	g := portGuard{allowed: map[int]bool{443: true}}
	d, _ := g.Decide(acl.DecideArgs{Req: connectReq("example.com:443")})
	if d.Result != acl.Allow {
		t.Fatalf("got %v, want allow", d.Result)
	}
	d, _ = g.Decide(acl.DecideArgs{Req: connectReq("example.com:80")})
	if d.Result != acl.Deny {
		t.Fatalf("got %v, want deny", d.Result)
	}
}

func TestParsePorts(t *testing.T) {
	if _, err := parsePorts("80,abc"); err == nil {
		t.Error("expected an error for a non-numeric port")
	}
	if _, err := parsePorts("0"); err == nil {
		t.Error("expected an error for port 0")
	}
	if _, err := parsePorts(" , "); err == nil {
		t.Error("expected an error for an empty list")
	}
	p, err := parsePorts("443, 80")
	if err != nil || !p[80] || !p[443] || len(p) != 2 {
		t.Errorf("got %v %v", p, err)
	}
}

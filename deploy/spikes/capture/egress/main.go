// Command shelfy-egress is Smokescreen (github.com/stripe/smokescreen, pinned in
// go.mod) with one addition: a destination port allowlist, 80 and 443 by default.
//
// Smokescreen already refuses every destination that is not a public unicast
// address after DNS resolution (loopback, RFC 1918, link-local and metadata,
// CGNAT, ULA, NAT64, 6to4, Teredo, its own addresses), plus the ranges listed in
// its config file. It has no port policy, so this wrapper adds one as an ACL
// decider that runs before the YAML egress ACL. With a decider in place,
// Smokescreen also refuses IPv6 literal destinations outright.
//
// The proxy has no client roles: every request gets the ACL's default rule.
//
// Usage: shelfy-egress --config-file /etc/shelfy-egress/config.yaml [smokescreen flags]
// Environment: SHELFY_EGRESS_ALLOWED_PORTS, comma-separated (default "80,443").
package main

import (
	"errors"
	"fmt"
	"log"
	"net/http"
	"os"
	"sort"
	"strconv"
	"strings"

	"github.com/sirupsen/logrus"
	"github.com/stripe/smokescreen/cmd"
	"github.com/stripe/smokescreen/pkg/smokescreen"
	acl "github.com/stripe/smokescreen/pkg/smokescreen/acl/v1"
	"github.com/stripe/smokescreen/pkg/smokescreen/hostport"
)

const defaultAllowedPorts = "80,443"

// portGuard refuses destinations whose port is not allowed, then defers to the
// inner decider (the YAML egress ACL), if any.
type portGuard struct {
	inner   acl.Decider
	allowed map[int]bool
}

func (g portGuard) Decide(args acl.DecideArgs) (acl.Decision, error) {
	port, err := destinationPort(args.Req)
	if err != nil {
		return acl.Decision{Result: acl.Deny, Reason: "destination port cannot be determined"}, nil
	}
	if !g.allowed[port] {
		return acl.Decision{
			Result: acl.Deny,
			Reason: fmt.Sprintf("destination port %d is not allowed", port),
		}, nil
	}
	if g.inner == nil {
		return acl.Decision{Result: acl.Allow, Reason: "destination port allowed"}, nil
	}
	return g.inner.Decide(args)
}

// destinationPort mirrors how Smokescreen derives the destination: the
// authority of a CONNECT request, or the Host and scheme of a plain HTTP proxy
// request (default port from the scheme).
func destinationPort(req *http.Request) (int, error) {
	if req == nil {
		return 0, errors.New("no request")
	}
	var (
		hp  hostport.HostPort
		err error
	)
	if req.Method == http.MethodConnect {
		hp, err = hostport.New(req.Host, false)
	} else {
		scheme := ""
		if req.URL != nil {
			scheme = req.URL.Scheme
		}
		hp, err = hostport.NewWithScheme(req.Host, scheme, false)
	}
	if err != nil {
		return 0, err
	}
	if hp.Port == hostport.NoPort {
		return 0, errors.New("no port")
	}
	return hp.Port, nil
}

func parsePorts(spec string) (map[int]bool, error) {
	if strings.TrimSpace(spec) == "" {
		spec = defaultAllowedPorts
	}
	ports := map[int]bool{}
	for _, field := range strings.Split(spec, ",") {
		field = strings.TrimSpace(field)
		if field == "" {
			continue
		}
		port, err := strconv.Atoi(field)
		if err != nil || port < 1 || port > 65535 {
			return nil, fmt.Errorf("invalid port %q in SHELFY_EGRESS_ALLOWED_PORTS", field)
		}
		ports[port] = true
	}
	if len(ports) == 0 {
		return nil, errors.New("SHELFY_EGRESS_ALLOWED_PORTS lists no port")
	}
	return ports, nil
}

func noRole(*http.Request) (string, error) {
	return "", smokescreen.MissingRoleError("shelfy-egress has no client roles")
}

func main() {
	ports, err := parsePorts(os.Getenv("SHELFY_EGRESS_ALLOWED_PORTS"))
	if err != nil {
		logrus.Fatal(err)
	}
	conf, err := cmd.NewConfiguration(nil, nil)
	if err != nil {
		logrus.Fatalf("could not create configuration: %v", err)
	}
	if conf == nil {
		return // --help or --version was handled
	}
	conf.RoleFromRequest = noRole
	conf.AllowMissingRole = true
	conf.EgressACL = portGuard{inner: conf.EgressACL, allowed: ports}

	conf.Log.Formatter = &logrus.JSONFormatter{}
	log.SetOutput(&smokescreen.Log2LogrusWriter{Entry: conf.Log.WithField("stdlog", "1")})
	log.SetFlags(0)

	list := make([]int, 0, len(ports))
	for p := range ports {
		list = append(list, p)
	}
	sort.Ints(list)
	conf.Log.WithField("allowed_ports", list).Info("shelfy-egress port allowlist")

	smokescreen.StartWithConfig(conf, nil)
}

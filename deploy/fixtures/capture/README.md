The existing SSRF fixture origin serves `probe.html` at `/capture/probe.html`
and the deterministic capture corpus at `/web-capture/`. Only the egress proxy
joins its subnet. Capture's positive control must pass through that proxy;
the fixture subnet is never added to capture's network.

`isolation-check.mjs` runs inside the capture image after the stack is healthy.
The host launcher checks Docker's isolated gateway mode before executing it.
No token or capture input from a real user is used or printed.

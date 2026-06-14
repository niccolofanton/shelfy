# Security Policy

## Supported versions

Security fixes are applied to the latest released version of Shelfy. Always update to the
most recent release before reporting an issue.

## Reporting a vulnerability

**Please do not open a public issue for security vulnerabilities.**

Report privately using **GitHub Security Advisories** ("Report a vulnerability" on the
repository's *Security* tab). If that is unavailable, contact the maintainer through the
private contact listed on the repository profile.

Please include:

- a description of the vulnerability and its impact;
- steps to reproduce (proof-of-concept if possible);
- affected version / commit and platform.

We aim to acknowledge reports within a few days and to provide a remediation timeline after
triage. Please give us a reasonable window to release a fix before any public disclosure
(coordinated disclosure).

## Scope notes

Shelfy is a local-first desktop app that automates the user's own authenticated social
sessions and runs local AI sidecars. Of particular interest:

- Electron IPC / preload surface and webview navigation.
- Handling of untrusted remote content during capture (SSRF, injection).
- Integrity of binaries and model weights downloaded at runtime.

Out of scope: issues that require a already-compromised host, or the inherent risk of
automating a platform against its Terms of Service (see [DISCLAIMER.md](DISCLAIMER.md)).

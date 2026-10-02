# Shelfy online — execution log

Live status of [IMPLEMENTATION-PLAN.md](IMPLEMENTATION-PLAN.md). The plan is the spec; this file records how the work runs, what is done, and what waits on the owner.

## How the work runs

- **Integration branch:** `web/foundations`, branched from `dev`. Every task lands on it as a fast-forward and is pushed; CI must be green.
- **Lanes:** one task per lane. A lane is a Claude Code subagent in its own git worktree, on branch `web/<task>-<slug>`, started from the tip of `web/foundations`.
- **Lead:** one session plans the waves, reviews each lane (diff, checks, and an independent reviewer for substantive tasks), integrates it and updates this file.
- **Concurrency:** at most 2 lanes at once, because of the local disk budget.

## Lane rules

1. Read the plan sections your task cites before writing code. The plan is the spec.
2. Branch from the integration tip: `git switch -c web/<task>-<slug> web/foundations`.
3. Run `pnpm install --frozen-lockfile` once; it installs the git hooks and lets you run the desktop checks.
4. Rust uses the toolchain pinned in `rust-toolchain.toml`. Keep the preset `CARGO_TARGET_DIR`; cargo may wait on its build lock.
5. Scope is additive (D25): the desktop app (`electron/`, `src/`, root build config) stays untouched unless the task says otherwise. Do not change the root `package.json` or `pnpm-lock.yaml` unless the task needs a dependency, and say so.
6. Commits: Conventional Commits in English, header ≤ 100 characters, no `Co-Authored-By` trailer. Stage explicit paths, never `git add -A`, never `--no-verify`, never `git stash` (the stash is shared across worktrees). Do not push or merge.
7. Before finishing, rebase on `web/foundations` if it moved, re-run the checks, and leave the worktree clean.
8. When the plan does not cover a decision, pick the plan-consistent option and report it as an assumption. Do not stop.
9. Privacy: the reference library snapshot (outside the repo, in `../shelfy-web-local/ref/`) is the owner's personal data. Read it read-only and never commit its content; fixtures are synthetic or scrubbed, and only aggregate counts appear in docs.
10. A lane that runs a server uses its own data dir (`../shelfy-web-local/data/<task>`) and port `18080 + n`, where `n` is the task number.

## Decisions during execution

These owner decisions override the plan where they conflict.

| # | Date | Decision | Replaces in the plan |
|---|---|---|---|
| E1 | 2026-10-02 | No throwaway servers. SPIKE-2, 4, 10 and 11 and the P5 capacity run happen on the osn VPS itself, inside resource-limited containers. | D20 "throwaway CX33s"; §6.3; §9 methods |
| E2 | 2026-10-02 | The lead may deploy, restart and reconfigure on the osn VPS without asking. The one hard constraint: Hermes, the AI agent, must keep working. Restarting it is fine. | Appendix B sign-offs |
| E3 | 2026-10-02 | The extension is tested as an unpacked build. No Chrome Web Store submission for now. | SPIKE-7; the store steps in P2 and P5 |
| E4 | 2026-10-02 | Owner-only until further notice: no invites and no closed beta. The owner account is created with the admin CLI; its email lives in the osn secrets, not in this repo. | P2 closed beta; P5 invites |

## Status

### P0 — Foundations and spikes

| Task | What | Needs | Status | Branch |
|---|---|---|---|---|
| T1 | Cargo workspace, pinned toolchain, `deny.toml`, `deploy/` skeleton, CI `rust` job | — | done | `web/t1-workspace` (b0baaf5…bad7139) |
| T2 | SPIKE-1: legacy reader, canonical keys, `shelfy-migrate plan` | T1 | running | `web/t2-legacy-reader` |
| T3 | Schema v1, `UserDb`, repositories, FTS maintenance, golden harness | T1 | running | `web/t3-schema-v1` |
| T4 | SPIKE-5: FTS relevance against `search-eval` | T3 | todo | |
| T5 | SPIKE-3 build: minimal MV3 extension and comparison tooling | — | running | `web/t5-extension-spike` |
| T6 | SPIKE-2 and SPIKE-10 on the osn VPS (E1) | — | running | `web/t6-spikes-vps` |
| T7 | `crates/server`: axum app, config, health, metrics, errors, OpenAPI, admin CLI | T3 | todo | |
| T8 | `crates/media`: CAS, renditions, ThumbHash, `/media/*` | T7 | todo | |
| T9 | Migration v0 and the reference library installed locally | T2, T3, T8 | todo | |
| T10 | Owner auth v0: magic link, sessions, CSRF | T7 | todo | |
| T11 | Read API and generated TS client | T4, T7 | todo | |
| T12 | SPA slice behind `ShelfyClient` | T10, T11 | todo | |

### P1–P6

Each phase is broken down into tasks when the previous one is close to done.

## Owner actions

| # | Action | Needed by | Status |
|---|---|---|---|
| O1 | Load the unpacked extension and run the SPIKE-3 comparison on your own accounts | P2 | after T5 |
| O2 | Appendix B prerequisites: DNS Edit on the OpenTofu token; R2 bucket `osn-backups` with a scoped token | first osn PR (P1) | pending |
| O3 | Optional: fix Homebrew permissions so local tools such as mailpit can be installed | T10 | optional |

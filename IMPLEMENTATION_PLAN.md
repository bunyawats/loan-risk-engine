# IMPLEMENTATION_PLAN — loan-risk-engine

Task list derived from `SPEC.md` §12, adapted to this standalone repo (see `CLAUDE.md`).
A box is ticked only when its DoD is met. Rules and integration tasks also need the live
E2E check in the POC stack; until then they carry a dated status note instead.

## Current Status

**Next task:** the real Jev call for B2 (O1), then C2 in the POC repo.

Phases A and B are code-complete with tests green. Tasks that touch rules or integration stay unticked until the live E2E (C3) has run.

## Phase A — Skeleton + parity (no Jev)

- [x] A1. Scaffold: deps, `lib.rs` + `main.rs`, config, tracing, `/healthz`, `.env.example`.
- [x] A2. `model.rs`: typed request, `RiskTier` enum; invalid input → MEDIUM.
- [ ] A3. `rules.rs`: ZEN loader with `spawn_blocking`; `rules/risk_tier.v1.json`.
  - *2026-10-02: implemented, tests green; live E2E pending Phase C.*
- [ ] A4. `webhook.rs` + `/assess` handler (blocking semantics, deadline).
  - *2026-10-02: implemented, contract tests green, smoke-tested against a local sink; live E2E pending Phase C.*
- [ ] A5. Parity and contract tests green. DoD: identical tiers to the mock for the full boundary table.
  - *2026-10-02: `tests/parity.rs` and `tests/contract.rs` green; live E2E pending Phase C.*

## Phase B — Features + Jev

- [x] B1. `features.rs` and tests.
- [ ] B2. `jev.rs`: Typesafe System One client, timeout and fallback.
  - *2026-10-02: implemented and wiremock-tested. Blocked on one real call to confirm the answer field names and the `jev-1.13` model id (O1).*
- [ ] B3. `rules/risk_tier.v2.json` plus invariant tests I1–I5.
  - *2026-10-02: table as SPEC §7.3, proptest invariants green; live E2E pending Phase C.*
- [x] B4. Audit log line.

## Phase C — Packaging and swap

- [x] C1. Dockerfile and local `docker-compose.yml` with a stub decisions sink.
  - *2026-10-02: image builds (200MB, runs as uid 10001); compose smoke test passed for v1 and v2.*
- [ ] C2. POC: compose service added, mock behind the `mock` profile, KrakenD `/assess` host switched. *(POC repo; not started)*
- [ ] C3. Live E2E with `RULES_VERSION=v1`, then `v2` with `JEV_ENABLED=true`. *(POC stack; not started)*
- [x] C4. CI: fmt, clippy, test.
  - *2026-10-02: first run green on GitHub.*

## Phase D — Docs (POC side)

- [ ] D1. POC `CLAUDE.md` and `risk-assessment-nats` skill updated.
- [ ] D2. POC Known Gaps updated.
- [ ] D3. Remove `mock_risk_engine/` once confirmed.

## Open decisions

- **O1.** Jev request shape confirmed from the author's MCP server (`POST /v1/systemone`, `{state, model, questions}`). The per-answer response fields and the validity of the pinned `jev-1.13` id still need one real call.
- **O2.** v2 thresholds (LTV 0.97, LTI 1.0, Jev cut-offs 0.5 / 0.6) are placeholders awaiting a human decision.
- **O3.** Whether a Jev-flagged LOW→MEDIUM should be visible to the Underwriter.
- **O4.** Mayan document checks: out of scope.
- **O5.** v2 auto-approves a small loan whose ratio could not be computed (e.g. personal loan with `monthly_income` ≤ 0 gives a null loan-to-income, so H3 cannot fire and L1 can). Should a null required feature force MEDIUM? Needs a human decision; would be a v3 table.

## Session Log

- **2026-10-02** — Contract checked against `loan-onboarding-poc@737023b`. `ASSESS_DEADLINE_MS` default lowered to 4000 (adapter HTTP timeout is 5s). Plan approved for Phases A, B, C1, C4; POC edits and live E2E deferred.
- **2026-10-02** — Phases A and B implemented in one pass (45 tests). Decisions: raw-body handler so `/assess` never returns 4xx; `R-DEADLINE` and `R-RULES-ERROR` fallbacks; `JEV_API_URL` defaults to the Typesafe System One endpoint; `state` sent to Jev as a JSON string; audit line nests `signals`/`features`/`latency_ms` as JSON strings. zen-engine 2.1.1: null inputs compare as false in table cells (covered by the I1 property test). cargo needed `CARGO_HTTP_MULTIPLEXING=false` to download on this network.
- **2026-10-02** — C1 verified with Docker. Host port made overridable (`RISK_ENGINE_HOST_PORT`) because the POC stack publishes Mayan on 8000. C4 confirmed by the first green CI run.

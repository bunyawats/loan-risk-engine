# IMPLEMENTATION_PLAN — loan-risk-engine

Task list derived from `SPEC.md` §12, adapted to this standalone repo (see `CLAUDE.md`).
A box is ticked only when its DoD is met. Rules and integration tasks also need the live
E2E check in the POC stack; until then they carry a dated status note instead.

## Current Status

**Next task:** none open in Phases A–C. Remaining: D3 (mock removal, deferred by the owner) and the open decisions below.

Rules v1 is live in the POC stack and verified end to end. Rules v2 and the Jev client are code-complete and tested with mocks, but not yet run live.

## Phase A — Skeleton + parity (no Jev)

- [x] A1. Scaffold: deps, `lib.rs` + `main.rs`, config, tracing, `/healthz`, `.env.example`.
- [x] A2. `model.rs`: typed request, `RiskTier` enum; invalid input → MEDIUM.
- [x] A3. `rules.rs`: ZEN loader with `spawn_blocking`; `rules/risk_tier.v1.json`.
  - *2026-10-02: implemented, tests green; live E2E passed with v1 (C3).*
- [x] A4. `webhook.rs` + `/assess` handler (blocking semantics, deadline).
  - *2026-10-02: implemented, contract tests green, smoke-tested against a local sink; live E2E passed with v1 (C3).*
- [x] A5. Parity and contract tests green. DoD: identical tiers to the mock for the full boundary table.
  - *2026-10-02: `tests/parity.rs` and `tests/contract.rs` green; live E2E passed with v1 (C3).*

## Phase B — Features + Jev

- [x] B1. `features.rs` and tests.
- [x] B2. `jev.rs`: Typesafe System One client, timeout and fallback.
  - *2026-10-02: implemented and wiremock-tested. Confirmed against the real API the same day: request accepted, parser fixed to the real answer fields (`noul`, `choice`).*
- [x] B3. `rules/risk_tier.v2.json` plus invariant tests I1–I5.
  - *2026-10-02: table as SPEC §7.3, proptest invariants green; live E2E pending Phase C.*
- [x] B4. Audit log line.

## Phase C — Packaging and swap

- [x] C1. Dockerfile and local `docker-compose.yml` with a stub decisions sink.
  - *2026-10-02: image builds (200MB, runs as uid 10001); compose smoke test passed for v1 and v2.*
- [x] C2. POC: compose service added, mock behind the `mock` profile, KrakenD `/assess` host switched.
  - *2026-10-02: edited in the POC working tree (uncommitted there); `docker compose config` valid and the `risk-engine` image builds from the POC compose file. Committed in the POC as `db810b7`. Superseded the same day: the engine now runs as a standalone container from this repo and the POC reaches it at `host.docker.internal:18000` (see Session Log).*
- [x] C3. Live E2E with `RULES_VERSION=v1`, then `v2` with `JEV_ENABLED=true`.
  - *2026-10-02: v1 passed in the POC stack via `scripts/generate_real_e2e_data.py` (10 applications): $5,000 and $8,000 → APPROVED; $60,000 → PENDING_UNDERWRITING → PENDING_MANAGER_APPROVAL → APPROVED; $150,000 → REJECTED. All ten webhooks returned 202, no adapter errors. *
  - *2026-10-02: v3 with `JEV_ENABLED=true` passed live (8 applications, all as expected): clean personal loan → APPROVED (L1); gambling purpose → underwriting (M3, purpose_high_risk 0.99); unstable employment → underwriting (M4); zero income → underwriting (F1); plausible vehicle → APPROVED (L1); gibberish/injection vehicle text → underwriting (M2, text_anomaly 0.98); $60,000 → underwriting → PENDING_MANAGER_APPROVAL (M7); $150,000 → REJECTED (H1). Jev latency 313–783 ms, all webhooks 202. v2 itself was not run live; v3 is a superset of it.*
- [x] C4. CI: fmt, clippy, test.
  - *2026-10-02: first run green on GitHub.*

## Phase D — Docs (POC side)

- [x] D1. POC `CLAUDE.md` and `risk-assessment-nats` skill updated.
  - *2026-10-02: also the POC README intro.*
- [x] D2. POC Known Gaps updated.
  - *2026-10-02: `known-gaps-and-gotchas` skill: thresholds now live in this repo's tables; new gaps for the second-repo build dependency and Jev.*
- [ ] D3. Remove `mock_risk_engine/` once confirmed.
  - *2026-10-02: owner decided to keep the mock as the rollback for now.*

## Open decisions

- **O1.** Jev request shape confirmed from the author's MCP server (`POST /v1/systemone`, `{state, model, questions}`). **Resolved 2026-10-02:** answer fields are `noul` and `choice`; the pin is `jev-1.13.0` (`jev-1.13` is rejected).
- **O2.** v2 thresholds (LTV 0.97, LTI 1.0, Jev cut-offs 0.5 / 0.6) are placeholders awaiting a human decision.
- **O3.** Whether a Jev-flagged LOW→MEDIUM should be visible to the Underwriter.
- **O4.** Mayan document checks: out of scope.
- **O5.** v2 auto-approves a small loan whose ratio could not be computed (e.g. personal loan with `monthly_income` ≤ 0 gives a null loan-to-income, so H3 cannot fire and L1 can). **Resolved 2026-10-02:** yes. `rules/risk_tier.v3.json` adds `F1`/`F2` (MEDIUM when the ratio is null); v2 is unchanged.

## Session Log

- **2026-10-02** — Contract checked against `loan-onboarding-poc@737023b`. `ASSESS_DEADLINE_MS` default lowered to 4000 (adapter HTTP timeout is 5s). Plan approved for Phases A, B, C1, C4; POC edits and live E2E deferred.
- **2026-10-02** — Phases A and B implemented in one pass (45 tests). Decisions: raw-body handler so `/assess` never returns 4xx; `R-DEADLINE` and `R-RULES-ERROR` fallbacks; `JEV_API_URL` defaults to the Typesafe System One endpoint; `state` sent to Jev as a JSON string; audit line nests `signals`/`features`/`latency_ms` as JSON strings. zen-engine 2.1.1: null inputs compare as false in table cells (covered by the I1 property test). cargo needed `CARGO_HTTP_MULTIPLEXING=false` to download on this network.
- **2026-10-02** — C1 verified with Docker. Host port made overridable (`RISK_ENGINE_HOST_PORT`) because the POC stack publishes Mayan on 8000. C4 confirmed by the first green CI run.
- **2026-10-02** — C2 edits made in the POC working tree: `risk-engine` service (build context `${RISK_ENGINE_BUILD_CONTEXT:-../../Rust/loan-risk-engine}`, `RISK_ENGINE_RULES_VERSION` default `v1`), `mock-risk-engine` behind `profiles: ["mock"]`, KrakenD `/assess` host → `http://risk-engine:8000`.
- **2026-10-02** — C3 (v1) run. The POC's `.venv` points at a removed Python 3.13, so the script was run with `uv run --no-project --with httpx python scripts/generate_real_e2e_data.py`.
- **2026-10-02** — Rules v3 added (`RULES_VERSION=v3`): v2 plus `F1`/`F2`. Invariant suite now runs over v2 and v3. Not run live; default stays `v1`.
- **2026-10-02** — Integration changed to a standalone container. This repo's compose file publishes host port 18000 and defaults `KRAKEND_URL` to `http://host.docker.internal:8090`; the stub sink moved behind the `stub` profile. The POC dropped its in-stack `risk-engine` service and its `krakend.json` `/assess` host is now `http://host.docker.internal:18000`. Live E2E re-run on v1 through the host ports: 10 applications, same outcomes as before, all webhooks 202, no engine or adapter errors.
- **2026-10-02** — Real Jev calls made. `jev-1.13` → HTTP 400 unknown model; `jev-latest` and `jev-1.13.0` → 200 in ~0.5s. `JEV_MODEL` default changed to `jev-1.13.0` on the owner's decision. Parser fixed from the assumed `probability`/`value` to the real `noul`/`choice`. Added `docs/component-architecture.md`.
- **2026-10-02** — C3 second half run with rules v3 + Jev (`jev-1.13.0`) through the standalone container. Observation for O2: a plain "Toyota Camry 2024" scored `vehicle_description_plausible` 0.62, only 0.12 above the 0.5 placeholder cut-off. The engine container was left running on v3 with Jev enabled.
- **2026-10-04** — Rustdoc comments and 16 doctests added. Published to crates.io as `loan-risk-engine` 0.1.0 (MIT OR Apache-2.0; package limited to code, rules, README, licenses), tagged `v0.1.0`, GitHub release created.
- **2026-10-04** — 0.1.1: a zero or negative `amount` is now invalid input (MEDIUM, `R-INVALID-INPUT`); before, v1 matched `amount < 15000` and auto-approved it. `serde_json` built with `arbitrary_precision` so a numeric decimal keeps every digit (`12345678901234567.89` used to become `...568`). No rules file changed; not run live in the POC stack.
- **2026-10-04** — 0.1.1 run live in the POC stack. Image rebuilt; the build OOM-killed the POC's Keycloak (7.7 GiB Docker VM), restarted with `docker compose up -d keycloak`. `generate_real_e2e_data.py` fails by design under v3 + Jev: it always sends purpose "Debt consolidation", which Jev scores as risky (`purpose_high_risk` 0.73 → M3), so its $8,000 auto-approval never happens (APP-431361645 left in PENDING_UNDERWRITING). Re-run on v1: passed, 10 applications ($5,000 and $8,000 → APPROVED; $60,000 → PENDING_UNDERWRITING → PENDING_MANAGER_APPROVAL → APPROVED; $150,000 → REJECTED), all webhooks 202, no engine warnings. Fixes checked on a throwaway container: `-5000` and `0` → MEDIUM `R-INVALID-INPUT`; numeric `12345678901234567.89` kept exact. Engine left on v1 with Jev off.

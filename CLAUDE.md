# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

Read this first, then `SPEC.md` (the full design), then `IMPLEMENTATION_PLAN.md` (what to do next).

---

## Current state

Phases A and B are implemented in this repo and green under `cargo test`: the v1 parity service, features, the Jev client, rules v2, and the audit log. `IMPLEMENTATION_PLAN.md` tracks what is ticked and what is waiting on the live E2E in the POC stack.

Not done yet: the Docker image has never been built (C1 is written but unverified), nothing in the POC has been changed (C2), and no live E2E has run (C3).

`SPEC.md` was drafted when the service was going to live inside the POC repo as `risk_engine_rs/`. Where the two differ, **this file wins**: the service is this standalone repo, `/healthz` includes `rules_sha256`, and `ASSESS_DEADLINE_MS` defaults to `4000` (not `10000`).

---

## What this is

A standalone **risk-assessment microservice** for the `loan-onboarding-poc` project (github.com/bunyawats/loan-onboarding-poc). It replaces that project's Python `mock_risk_engine` and decides a loan application's `risk_tier` (`LOW | MEDIUM | HIGH`) using:

1. **Deterministic features**: ratios computed from the application (loan-to-income, LTV, down-payment ratio).
2. **Jev** (Typesafe AI decision model): fuzzy judgments over free-text payload fields. It returns **signals only, never a tier**.
3. **ZEN Engine** (GoRules, `zen-engine` crate): versioned JSON decision tables that combine features and signals into the final tier.

**Stack:** Rust (stable), Axum, Tokio, `zen-engine`, `reqwest` (rustls), `rust_decimal`, `tracing`. Shipped as one Docker image and run as its own container.

This repo has **no dependency on the POC codebase**. The two meet only at the HTTP contract below. The POC treats this service the way it treats Mayan or Keycloak: as a genuinely external system.

---

## The contract (owned by the consumer — do not change unilaterally)

The POC's `risk-adapter` calls this service through **KrakenD**, and this service calls back through KrakenD. Any change here needs a matching change in the POC repo, so treat the contract as frozen.

### Inbound: `POST /assess` on port `8000`

```json
{
  "application_id": "string",
  "applicant_identifier": "string",
  "product_type": "personal_loan | auto_loan | mortgage",
  "amount": "decimal as string, e.g. \"60000.00\"",
  "payload": { }
}
```

- Respond **`202`** with an empty body, **after** the decision webhook has been called (blocking semantics, parity with the mock). Whole-request deadline: `ASSESS_DEADLINE_MS`.
- **The deadline must stay under 5 seconds.** The POC's `risk-adapter` calls `/assess` with a default `httpx.AsyncClient()` (5s timeout), and KrakenD's own timeout is 10s. A slower assessment still delivers its decision through the webhook, but the adapter logs "Risk Engine /assess unreachable", which is misleading. `SIMULATED_DELAY_SECONDS` and `JEV_TIMEOUT_MS` both count against this budget.
- **Payload by product type.** Decimals arrive as strings; parse them with `rust_decimal`, never `f64`.
  - `personal_loan`: `purpose`, `employment_status`, `monthly_income`
  - `auto_loan`: `vehicle_make_model`, `vin`, `down_payment`
  - `mortgage`: `property_address`, `appraised_value`, `down_payment`
- If the decide stage overruns the deadline, the decision is `MEDIUM` (rule `R-DEADLINE`) and the webhook is still posted (2s timeout of its own).
- Invalid or unparsable input is **not** a 4xx. Decide `MEDIUM` (rule `R-INVALID-INPUT`) and still return 202, so the application reaches a human instead of getting stuck. This is a deliberate difference from the mock, which answers a bad `amount` with a 500 and never sends a decision. The one exception: a body with no readable `application_id` cannot be routed anywhere, so it gets a `warn` log and a 202 with no webhook call.

### Outbound: `POST {KRAKEND_URL}/decisions`

```json
{ "application_id": "string", "risk_tier": "LOW | MEDIUM | HIGH" }
```

- The POC forwards `risk_tier` **unvalidated** into a Temporal signal, and an unknown value crashes that signal. `risk_tier` is therefore always the `RiskTier` enum, never a free string.
- If the webhook fails (non-2xx or transport error), log at `error` and still return 202. Never panic.

### Additive (not part of the contract)

`GET /healthz` returns 200 with `{rules_version, rules_sha256, jev_enabled}`. It is not routed through KrakenD.

### How the POC's workflow uses the tier (context only, lives in the POC)

| tier | POC result |
|---|---|
| `LOW` | auto **APPROVED** (terminal) |
| `MEDIUM` | **PENDING_UNDERWRITING** (human review) |
| `HIGH` | auto **REJECTED** (terminal) |

The POC escalates human-approved loans of **≥ $50,000** (`MANAGER_ESCALATION_THRESHOLD_USD`) to a Manager. That path is only reachable when some amounts ≥ $50,000 come back `MEDIUM`.

---

## Non-negotiable invariants (each one has a test; never weaken the test)

- **I1** Output ∈ {LOW, MEDIUM, HIGH}. Any internal error or unexpected rules output becomes `MEDIUM` (rule `R-RULES-ERROR`).
- **I2** **Jev can only push toward human review.** A Jev signal may turn LOW into MEDIUM, and nothing else. `HIGH` (auto-reject) and `LOW` (auto-approve) come only from deterministic conditions.
- **I3** **Jev unavailable means no auto-approve.** On timeout, error, or `JEV_ENABLED=false` while rules v2 is active, LOW becomes MEDIUM.
- **I4** **Manager path stays reachable.** Some inputs with `50,000 ≤ amount < HIGH cut-off` must yield `MEDIUM`.
- **I5** **Invalid input → MEDIUM.**

If a requested change would violate one of these, stop and ask. Don't work around it.

---

## Repository layout

```
loan-risk-engine/
  CLAUDE.md  SPEC.md  IMPLEMENTATION_PLAN.md  README.md
  Cargo.toml  Dockerfile  docker-compose.yml  .env.example
  rules/
    risk_tier.v1.json     # parity with the POC's mock: <15k LOW, <100k MEDIUM, else HIGH
    risk_tier.v2.json     # features + Jev signals
  src/
    lib.rs       # AppState, build_router; everything tests import
    main.rs      # thin binary: config, rules load, tracing, graceful shutdown
    config.rs    # env parsing; fail fast on bad config at startup
    model.rs     # AssessRequest, Payload enum, RiskTier enum, Decision
    features.rs  # pure functions, no I/O
    jev.rs       # Jev HTTP client + per-product question sets
    rules.rs     # ZEN loader/evaluator
    webhook.rs   # /decisions client
    handler.rs   # orchestrates parse → features → jev → rules → webhook → audit log
  tests/
    parity.rs  rules_v2.rs  contract.rs
```

**Dependency direction:** `handler` → (`features`, `jev`, `rules`, `webhook`) → `model`. `features` and `model` do no I/O. Only `jev.rs` talks to Typesafe, and only `webhook.rs` talks to KrakenD.

---

## Commands

```bash
cargo run                                  # listens on :8000 (needs .env; JEV_ENABLED=false works offline)
cargo test                                 # all tests; Jev and KrakenD always mocked (wiremock)
cargo test --test parity                   # one integration test file (tests/parity.rs)
cargo test --test contract webhook_500     # tests in one file whose name contains a substring
cargo test features::                      # unit tests of one module
cargo test <name> -- --exact --nocapture   # a single test, with its output
cargo fmt --check && cargo clippy --all-targets -- -D warnings
docker build -t loan-risk-engine .
docker compose up                          # this service + a stub KrakenD/decisions sink for local dev

# smoke test
curl -s -XPOST localhost:8000/assess -H 'content-type: application/json' -d '{
  "application_id":"APP-1","applicant_identifier":"a@b.c","product_type":"personal_loan",
  "amount":"60000","payload":{"purpose":"home renovation","employment_status":"full-time","monthly_income":"8000"}}'
```

CI runs: fmt, clippy (`-D warnings`), and test. No Typesafe key is ever needed in CI.

---

## Configuration (env)

| var | default | notes |
|---|---|---|
| `PORT` | `8000` | contract; don't change |
| `KRAKEND_URL` | `http://krakend:8080` | webhook base |
| `RULES_VERSION` | `v1` | `v1` (parity) or `v2`; v2 only after Phase C sign-off |
| `RULES_DIR` | `/app/rules` | |
| `JEV_ENABLED` | `false` | kill switch |
| `JEV_API_URL` | `https://api.typesafe.ai/v1/systemone` | Typesafe System One endpoint |
| `TYPESAFE_API_KEY` | — | secret; `.env` only, never commit or log it. Required when `JEV_ENABLED=true` (startup fails without it) |
| `JEV_MODEL` | `jev-1.13` | **pinned**; upgrading is a deliberate change |
| `JEV_TIMEOUT_MS` | `2000` | |
| `ASSESS_DEADLINE_MS` | `4000` | keep below the `risk-adapter`'s 5s HTTP timeout |
| `SIMULATED_DELAY_SECONDS` | `0` | demo parity knob with the old mock (whose default is `1`); counts against the deadline |
| `RUST_LOG` | `info` | |

Every new variable goes into `.env.example` and this table in the same commit.

---

## Integration with loan-onboarding-poc

The POC consumes this service as a container. The only POC-side changes are:

1. **docker-compose.yml** (POC): add a `risk-engine` service. Use either `build: <path to this repo>` (local dev) or `image: ghcr.io/bunyawats/loan-risk-engine:<tag>` (published). The build path is relative to the POC's compose file: `../loan-risk-engine` for a sibling checkout, `../../Rust/loan-risk-engine` for the author's layout (`Projects/Python/loan-onboarding-poc` and `Projects/Rust/loan-risk-engine`). Move `mock-risk-engine` behind `profiles: ["mock"]` for rollback.
2. **krakend/krakend.json** (POC): change the `/assess` backend host from `http://mock-risk-engine:8000` to `http://risk-engine:8000`.
3. **Rollback:** revert that one KrakenD line and start the `mock` profile.

Nothing in the POC's Python code, NATS adapter, Temporal workflow, or DB changes. If a task here seems to need a POC code change, it's a contract change. Flag it and stop.

**Live end-to-end check (definition of done for any rules change):** in the POC stack, using its `scripts/generate_real_e2e_data.py` helpers:
- $5,000 → APPROVED
- $60,000 → PENDING_UNDERWRITING → Underwriter approves → PENDING_MANAGER_APPROVAL
- $150,000 → REJECTED

---

## Rules (ZEN decision tables)

- Tables live in `rules/` as JSON. They can be edited in the GoRules visual editor and should be reviewed as data, not code.
- **Released versions are immutable.** Any change goes into a new file (`risk_tier.v3.json`) and a new `RULES_VERSION`. The SHA-256 of the active file is logged with every decision.
- Hit policy is `first`. Order matters, and it is how I2 and I3 are guaranteed: every `HIGH` rule is deterministic, and every Jev-driven rule outputs `MEDIUM` and sits above the single default `LOW` rule.
- v1 must stay byte-for-byte equivalent in behavior to the POC mock's boundary table: `1, 14999.99 → LOW`; `15000, 49999.99, 50000, 99999.99 → MEDIUM`; `100000, 150000 → HIGH`. `tests/parity.rs` enforces this.
- Threshold values in v2 (LTV, loan-to-income, Jev cut-offs) are **placeholders awaiting a human decision**. Don't "tune" them on your own initiative.

**Gotcha:** `zen_engine::Decision::evaluate()` returns a **`!Send`** future (it uses `Rc` internally), so it can't be awaited directly in an Axum handler. Run it inside `tokio::task::spawn_blocking` with a `current_thread` runtime (or a dedicated `LocalSet` thread). Parse the rules JSON once at startup and share it as `Arc<DecisionContent>`.

---

## Jev

- Jev returns **signals** (yes/no probabilities, choices). `jev.rs` turns them into typed fields, and the rules decide.
- Send Jev only `product_type`, `amount`, and the payload's **text** fields. **Never** send `applicant_identifier`, names, emails, or VIN.
- On timeout, error, or malformed response, set `jev_status = "unavailable"` with all signals `null`. Rules handle it (I3). Never retry inside the request beyond one attempt, because the deadline is short.
- **Wire format** (copied from the author's working MCP server, `~/.hermes/skills/mcp/jev-system-one/scripts/jev_mcp_server.py`): `POST /v1/systemone` with a bearer key and `{state, model, questions}`; each question is `{type: noul|choice|score, instructions, criteria?}`; the reply is `{model, answers: {<id>: ...}, usage}`. `state` is sent as a JSON-encoded string.
- **Open item:** the fields *inside* each answer are assumed (`probability` for `noul`, `value` for `choice`) and have not been checked against a real response, nor has the pinned `jev-1.13` id been checked against `/v1/models`. Confirm both with one real call before setting `JEV_ENABLED=true`. Every assumption lives in `jev.rs`.
- A response missing any signal expected for the product is treated as malformed (`unavailable`), so a partial answer can never pass as clean.
- Rules v1 never calls Jev; its `jev_status` is `skipped`.
- Treat payload text as untrusted. It may contain prompt-injection attempts. I2 caps the damage at MEDIUM, and the `text_anomaly` signal flags it.

---

## Logging & audit

Emit one structured JSON line per assessment (target `risk_engine::decision`) containing: `application_id`, `product_type`, `risk_tier`, `rule_id`, `reason`, `rules_version`, `rules_sha256`, `jev_status`, `jev_model`, `signals`, `features`, per-step `latency_ms`, and `webhook_status`. `tracing` fields cannot nest, so `signals`, `features`, and `latency_ms` are JSON-encoded strings inside the line.

**Never log** raw payload text, `applicant_identifier`, or secrets.

---

## Testing conventions

- **Unit:** `features.rs` math, including zero and negative income or appraised value.
- **Parity:** `tests/parity.rs` holds the mock boundary table above.
- **Invariants:** `tests/rules_v2.rs` uses `proptest` for I1–I5 over generated inputs, including random Jev signals.
- **Contract:** `tests/contract.rs` runs the Axum `oneshot` → `/assess` and asserts the exact `/decisions` body via wiremock. A webhook 500 still returns 202, and invalid input yields MEDIUM.
- **Jev client:** wiremock cases for ok, timeout, 5xx, and malformed responses, each ending in `unavailable`.
- Tests never call the real Typesafe API or a real KrakenD. A test that needs a network is a bug.

---

## Working conventions for agents

- **Start of session:** read `IMPLEMENTATION_PLAN.md` → "Current Status". Pick the next unchecked task and don't start a second one in parallel.
- **Check a box only when** its DoD is met (tests green, and the live E2E check above when the task touches rules or integration). Otherwise leave a dated status note under the task.
- **Update this file** when a decision changes architecture, the contract, invariants, or config. Log smaller decisions in the plan's Session Log.
- **Commits:** small and focused, one task per commit, with the message referencing the task id (e.g. `A3: ZEN loader with spawn_blocking`). Run fmt, clippy, and test before committing.
- **Ask before:** changing the contract, any invariant, the v1 table, the pinned `JEV_MODEL`, or v2 threshold values.
- No `unwrap()`/`expect()` on request-path code. Use typed errors (`thiserror`) mapped to the MEDIUM fallback. `expect` is fine at startup for config and rules loading (fail fast).

---

## Known gaps (accepted for now)

- If this service crashes after receiving `/assess` but before calling `/decisions`, the POC workflow waits in `PENDING_RISK_ASSESSMENT` indefinitely. This is the POC's existing "no timeout" gap and isn't fixed here.
- No auth between KrakenD and this service (internal Docker network only, same as the mock).
- The v2 thresholds and Jev cut-offs are unconfirmed placeholders.
- Mayan document checks (e.g. income vs. payslip) are deliberately out of scope. They would need Mayan credentials in this service.

# SPEC — Rust Risk Engine (Jev + ZEN) replacing `mock_risk_engine`

**Status:** Draft for review · **Target repo:** `loan-onboarding-poc` · **Scope:** one standalone service swap

---

## 1. Summary

Replace the Python `mock_risk_engine` (amount-bucketing only) with a new standalone **Rust service, `risk_engine_rs`**, built on **Axum**, that decides `risk_tier` using:

1. **Deterministic features** computed from the application (ratios such as loan-to-income or LTV).
2. **Jev** (Typesafe decision model) for fuzzy judgments over the product payload's free text, for example a risky loan purpose or an implausible vehicle description.
3. **ZEN Engine** (GoRules, Rust crate `zen-engine`) decision tables, which combine features and Jev signals into the final `LOW | MEDIUM | HIGH`.

The service keeps the **exact HTTP contract** of `mock_risk_engine`. Nothing in `loan_onboarding/`, `risk_adapter/`, NATS, or the Temporal workflow changes.

## 2. Goals / non-goals

**Goals**
- G1. Drop-in replacement: same endpoint, same request body, same webhook call, same port.
- G2. **Parity mode first.** Rules version v1 reproduces the mock's amount thresholds exactly, so the swap can be verified with no behavior change.
- G3. Jev-enhanced rules (v2) that can only *move an application toward human review*, never auto-approve on AI judgment alone (see §7).
- G4. Every decision is traceable: rule id, rules version, Jev model, signals, and latency in a structured log.
- G5. Business rules live in a versioned JSON decision table that can be edited in the GoRules visual editor, not in code.

**Non-goals**
- Changing the workflow state machine, `VALID_RISK_TIERS`, the NATS Adapter, KrakenD topology, or the DB schema.
- Surfacing risk reasons in `bff_backoffice` UI (consistent with the Phase 21 decision: no badge or tier in the UI).
- Using Temporal from Rust. The Temporal workflow stays in Python.
- Reading Mayan documents (possible later phase, §13).

## 3. Current state (as-is)

```
workflow (PENDING_RISK_ASSESSMENT)
  → activity submit_risk_assessment → risk/service.py
  → POST risk-adapter:/assessments → NATS risk.assessment.submitted
  → risk-adapter → POST krakend:/assess → mock-risk-engine:8000/assess
  → mock decides tier → POST krakend:/decisions → risk-adapter → NATS risk.assessment.decided
  → risk-adapter → Temporal signal_risk_decision(risk_tier)
```

- Mock rule: `amount < 15,000 → LOW`; `< 100,000 → MEDIUM`; else `HIGH`.
- Workflow mapping (`workflows.py::_resolve_risk_decision`): `LOW → APPROVED` (terminal, actor `risk-engine-auto`); `HIGH → REJECTED` (terminal); `MEDIUM → PENDING_UNDERWRITING`.
- `MANAGER_ESCALATION_THRESHOLD_USD = 50,000`. The MEDIUM band `[50,000, 100,000)` is what keeps `PENDING_MANAGER_APPROVAL` reachable (see Known Gaps, resolved 2026-09-08).
- The Adapter forwards `risk_tier` **unvalidated**. An unknown value makes the workflow signal raise `ApplicationError`.

## 4. HTTP contract (unchanged; must be honored exactly)

### 4.1 Inbound — `POST /assess` (port **8000**, reached via KrakenD `/assess`)

```json
{
  "application_id": "string",
  "applicant_identifier": "string",
  "product_type": "personal_loan | auto_loan | mortgage",
  "amount": "string decimal, e.g. \"60000.00\"",
  "payload": { "...product-specific, see 4.3..." }
}
```

- Response: **`202 Accepted`**, empty body.
- **Blocking semantics (parity with mock):** the handler decides, calls the webhook, and only then returns 202. Overall deadline: `ASSESS_DEADLINE_MS` (default 10,000).
- Malformed body (missing field, unparsable `amount`, unknown `product_type`): still decide. Return 202 and emit **`MEDIUM`** with rule id `R-INVALID-INPUT`, so the application lands with a human instead of getting stuck in `PENDING_RISK_ASSESSMENT`. Log at `warn`.

### 4.2 Outbound — `POST {KRAKEND_URL}/decisions`

```json
{ "application_id": "string", "risk_tier": "LOW | MEDIUM | HIGH" }
```

- `risk_tier` **must** be one of the three literals. This is enforced by a Rust enum and serde rename, never a free string.
- Non-2xx or a transport error: log `error`, do not panic, still return 202 to the caller (parity with mock).
- Optional (v2+, off by default): an extra `decision_trace` object. The Adapter reads only `application_id` and `risk_tier`, so extra keys are ignored, but they travel through NATS. Keep this off unless you need it downstream.

### 4.3 Product payloads (from `application/schemas.py`)

| product_type | payload fields |
|---|---|
| `personal_loan` | `purpose: str`, `employment_status: str`, `monthly_income: decimal` |
| `auto_loan` | `vehicle_make_model: str`, `vin: str`, `down_payment: decimal` |
| `mortgage` | `property_address: str`, `appraised_value: decimal`, `down_payment: decimal` |

Decimals arrive as JSON strings (Pydantic `model_dump(mode="json")`). Parse them with `rust_decimal`, never `f64`, for boundary-exact comparisons.

## 5. Architecture

```
POST /assess (axum)
  │ 1. parse + validate → AssessRequest (typed per product_type)
  │ 2. features::compute()        deterministic ratios (rust_decimal)
  │ 3. jev::signals()             fuzzy judgments (HTTP → Typesafe), timeout + fallback
  │ 4. rules::evaluate()          ZEN decision table → {risk_tier, rule_id, reason}
  │ 5. webhook::post_decision()   POST KRAKEND_URL/decisions
  │ 6. audit log (JSON line)
  └─ 202
```

### 5.1 Module layout

```
risk_engine_rs/
  Cargo.toml
  Dockerfile
  rules/
    risk_tier.v1.json      # parity: amount-only (mirrors mock)
    risk_tier.v2.json      # features + Jev signals
  src/
    main.rs        # axum router, config, tracing, graceful shutdown
    config.rs      # env parsing (§9)
    model.rs       # AssessRequest, Payload enum, RiskTier enum, Decision
    features.rs    # pure fns: loan_to_income, ltv, down_payment_ratio
    jev.rs         # Jev HTTP client + question sets per product
    rules.rs       # ZEN loader/evaluator (spawn_blocking wrapper)
    webhook.rs     # decisions client
    handler.rs     # orchestrates steps 1–6
  tests/
    parity.rs      # v1 boundary table == mock's test table
    rules_v2.rs
    contract.rs    # axum + wiremock: /assess → /decisions body
```

### 5.2 Crates

`axum`, `tokio`, `serde`/`serde_json`, `rust_decimal` (serde-str), `reqwest` (rustls), `zen-engine = "2"`, `tracing` + `tracing-subscriber` (json), `thiserror`/`anyhow`; dev: `wiremock`, `tower` (`ServiceExt::oneshot`).

### 5.3 Known technical constraint

`zen_engine::Decision::evaluate()` returns a **`!Send` future** (it uses `Rc` internally), so it cannot be awaited directly inside an Axum handler. Run it in `tokio::task::spawn_blocking` with a `current_thread` runtime, or on a dedicated `LocalSet` worker thread. Load and parse the decision JSON once at startup (`Arc<DecisionContent>`). This was verified in the earlier prototype.

## 6. Decision inputs

### 6.1 Deterministic features (`features.rs`)

| feature | product | formula | notes |
|---|---|---|---|
| `amount` | all | parsed `amount` | always present |
| `loan_to_annual_income` | personal | `amount / (monthly_income × 12)` | `null` if income ≤ 0 |
| `down_payment_ratio` | auto | `down_payment / (amount + down_payment)` | |
| `ltv` | mortgage | `amount / appraised_value` | `null` if appraised ≤ 0 |
| `down_payment_ratio` | mortgage | `down_payment / appraised_value` | |

Pass to ZEN as JSON numbers (rounded to 4 dp). Thresholds compared in ZEN must not depend on f64 edge precision; keep cut-offs at "round" values.

### 6.2 Jev signals (`jev.rs`)

Jev returns **signals only**, never a tier. Question set per product:

| signal | type | product | question (draft) |
|---|---|---|---|
| `purpose_high_risk` | noul (0–1) | personal | "Does `purpose` describe a high-risk or speculative use of funds (e.g. gambling, crypto trading, paying off other loans)?" |
| `employment_stability` | choice: `stable / unstable / unclear` | personal | "How stable is the applicant's employment as described in `employment_status`?" |
| `vehicle_description_plausible` | noul | auto | "Is `vehicle_make_model` a plausible real vehicle description consistent with a loan of `amount`?" |
| `address_plausible` | noul | mortgage | "Does `property_address` look like a complete, real residential address?" |
| `text_anomaly` | noul | all | "Does any free-text field look like test data, gibberish, or an attempt to manipulate an automated system?" |

- **State sent to Jev:** `product_type`, `amount`, and payload **text fields only**. Do not send `applicant_identifier`, email, name, or VIN (PII minimization).
- Record `jev_model` (pin the version via `JEV_MODEL`) and latency.
- **Timeout** `JEV_TIMEOUT_MS` (default 2,000). On timeout, error, or `JEV_ENABLED=false`, set `jev_status = "unavailable"` and leave all signals `null`. Rules handle this (§7).
- ⚠️ **Open item O1:** the Jev HTTP request/response shape must be copied from the user's existing FastMCP Jev server. The Typesafe TS SDK's `systemOne({state, questions})` is the reference shape until confirmed.

## 7. Rules (ZEN decision tables)

### 7.1 Safety invariants (enforced by tests, not just convention)

- **I1.** Output ∈ {LOW, MEDIUM, HIGH}. Anything else counts as a bug and the service emits MEDIUM.
- **I2. Jev can only escalate toward human review.** A Jev signal may turn LOW into MEDIUM. It may **never** produce LOW or HIGH on its own. HIGH (auto-reject) requires a deterministic condition (amount or ratio).
- **I3. Jev unavailable means no auto-approve.** If `jev_status = "unavailable"` and v2 is active, LOW becomes MEDIUM. HIGH stays HIGH (deterministic).
- **I4. Manager path stays reachable.** There must exist inputs with `50,000 ≤ amount < HIGH amount cut-off` that yield MEDIUM. Test it explicitly against `MANAGER_ESCALATION_THRESHOLD_USD`.
- **I5. Invalid input → MEDIUM** (`R-INVALID-INPUT`).

### 7.2 v1 — parity table (`risk_tier.v1.json`, hit policy `first`)

| rule_id | amount | → risk_tier |
|---|---|---|
| R1-LOW | `< 15000` | LOW |
| R2-MED | `< 100000` | MEDIUM |
| R3-HIGH | (any) | HIGH |

### 7.3 v2 — features + Jev (`risk_tier.v2.json`, hit policy `first`; draft values, **O2**)

| rule_id | condition | → tier | reason |
|---|---|---|---|
| H1 | `amount >= 100000` | HIGH | amount over auto-reject cap |
| H2 | mortgage and `ltv > 0.97` | HIGH | LTV too high |
| H3 | personal and `loan_to_annual_income > 1.0` | HIGH | loan exceeds annual income |
| M1 | `jev_status == "unavailable"` | MEDIUM | AI signals unavailable → human |
| M2 | `text_anomaly >= 0.5` | MEDIUM | suspicious input |
| M3 | personal and `purpose_high_risk >= 0.6` | MEDIUM | risky purpose |
| M4 | personal and `employment_stability != "stable"` | MEDIUM | employment unclear |
| M5 | auto and `vehicle_description_plausible < 0.5` | MEDIUM | vehicle implausible |
| M6 | mortgage and `address_plausible < 0.5` | MEDIUM | address implausible |
| M7 | `amount >= 15000` | MEDIUM | standard human review band |
| L1 | (default) | LOW | small, clean application |

Ordering guarantees I2 and I3: every HIGH rule is deterministic, and every Jev rule outputs MEDIUM and sits above the only LOW rule.

### 7.4 Versioning

- Active table selected by `RULES_VERSION` env (`v1` | `v2`); default **`v1`** until Phase C sign-off.
- `rules_version` string (e.g. `risk_tier@v2`) plus a SHA-256 of the JSON file is logged with every decision.
- Any edit to a table means a new file version. Never mutate a released version in place.

## 8. Observability / audit

One JSON log line per assessment (`tracing`, target `risk_engine::decision`):

```json
{"application_id":"…","product_type":"auto_loan","risk_tier":"MEDIUM","rule_id":"M5",
 "reason":"vehicle implausible","rules_version":"risk_tier@v2","rules_sha256":"…",
 "jev_status":"ok","jev_model":"jev-1.13","signals":{"vehicle_description_plausible":0.31,"text_anomaly":0.02},
 "features":{"amount":"32000","down_payment_ratio":0.1},"latency_ms":{"jev":180,"rules":2,"total":205},
 "webhook_status":202}
```

Never log the raw payload text or PII. Log signals and features only.

`GET /healthz` returns 200 (`{"rules_version":…,"jev_enabled":…}`). This is additive and not routed through KrakenD.

## 9. Configuration (env)

| var | default | purpose |
|---|---|---|
| `PORT` | `8000` | listen port (contract) |
| `KRAKEND_URL` | `http://krakend:8080` | webhook base (contract) |
| `RULES_VERSION` | `v1` | active decision table |
| `RULES_DIR` | `/app/rules` | table location |
| `JEV_ENABLED` | `false` | master switch for Jev calls |
| `JEV_API_URL` | — | Typesafe endpoint (**O1**) |
| `TYPESAFE_API_KEY` | — | secret; from `.env`, never committed |
| `JEV_MODEL` | `jev-1.13` | pinned model id |
| `JEV_TIMEOUT_MS` | `2000` | per-call timeout |
| `ASSESS_DEADLINE_MS` | `10000` | whole-request deadline |
| `SIMULATED_DELAY_SECONDS` | `0` | parity knob with the mock (demo only) |
| `RUST_LOG` | `info` | log level |

Add the new keys to `.env.example`.

## 10. Deployment

- **Dockerfile:** multi-stage. `rust:1-slim` builder (cargo-chef layer caching), then `debian:bookworm-slim` runtime with `ca-certificates`. Copy `rules/`. Run as a non-root user. `EXPOSE 8000`.
- **docker-compose.yml:** add service `risk-engine` (`depends_on: [krakend]`, ordering only, same comment pattern as the mock). Move `mock-risk-engine` behind `profiles: ["mock"]` for rollback.
- **krakend/krakend.json:** change the `/assess` backend host from `http://mock-risk-engine:8000` to `http://risk-engine:8000`. Rollback means reverting this one line plus the profile.
- Keep `mock_risk_engine/` in the repo until Phase D is done.

## 11. Testing

| level | what | where |
|---|---|---|
| Unit | `features.rs` math, incl. zero/negative income and appraised value | `src/features.rs` `#[cfg(test)]` |
| Parity | v1 table vs the **exact** mock boundary table: 1, 14999.99 → LOW; 15000, 49999.99, 50000, 99999.99 → MEDIUM; 100000, 150000 → HIGH | `tests/parity.rs` |
| Invariants | I1–I5 as property tests over generated inputs (e.g. `proptest`): Jev signals can never yield LOW→HIGH or MEDIUM→LOW | `tests/rules_v2.rs` |
| Contract | `oneshot` POST `/assess`; wiremock KrakenD asserts the exact `/decisions` body; webhook 500 → still 202 | `tests/contract.rs` |
| Jev client | wiremock Typesafe: ok, timeout, 5xx, malformed → `unavailable` | `src/jev.rs` tests |
| Live E2E | existing `scripts/generate_real_e2e_data.py` flow: $5k → APPROVED; $60k → PENDING_UNDERWRITING → Underwriter approve → PENDING_MANAGER_APPROVAL; $150k → REJECTED | manual DoD, logged in IMPLEMENTATION_PLAN |

**CI:** new job `risk-engine-rs` in `.github/workflows/ci.yml` running `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test` (no Typesafe key needed; Jev is always mocked in CI).

## 12. Implementation plan (checkbox-ready for `IMPLEMENTATION_PLAN.md`)

**Phase A — Skeleton + parity (no Jev)**
- [ ] A1. Scaffold `risk_engine_rs/` crate, config, tracing, `/healthz`.
- [ ] A2. `model.rs` typed request and `RiskTier` enum; invalid input → MEDIUM.
- [ ] A3. `rules.rs` ZEN loader with spawn_blocking; `risk_tier.v1.json`.
- [ ] A4. `webhook.rs` plus `/assess` handler (blocking semantics, deadline).
- [ ] A5. Parity and contract tests green. DoD: identical tiers to mock for the full boundary table.

**Phase B — Features + Jev**
- [ ] B1. `features.rs` and tests.
- [ ] B2. `jev.rs`, matched to the existing FastMCP server's call (**O1**); timeouts and fallback.
- [ ] B3. `risk_tier.v2.json` plus invariant tests I1–I5.
- [ ] B4. Audit log line (§8).

**Phase C — Swap in the stack**
- [ ] C1. Dockerfile and compose service; mock moved behind the `mock` profile.
- [ ] C2. KrakenD `/assess` backend switched.
- [ ] C3. Live E2E (§11) with `RULES_VERSION=v1`, then with `v2` and `JEV_ENABLED=true`.
- [ ] C4. CI job added.

**Phase D — Docs**
- [ ] D1. CLAUDE.md "Automated risk assessment" section and the `risk-assessment-nats` skill updated (real engine replaces mock; invariants I1–I5).
- [ ] D2. Known Gaps: the threshold-values question now points at the decision tables. The new gap is the Jev dependency, and the fallback behavior is documented.
- [ ] D3. Remove `mock_risk_engine/` (or keep it as a documented rollback) once the user confirms.

## 13. Open decisions

- **O1.** Jev HTTP contract: copy it from the existing FastMCP Jev server.
- **O2.** v2 threshold values (LTV 0.97, LTI 1.0, Jev cut-offs 0.5 and 0.6) are placeholders. They need a human decision, the same "assumed default, not yet confirmed" status as today's $15k/$100k.
- **O3.** Should a Jev-flagged LOW→MEDIUM be distinguishable to the Underwriter? Currently no, per the Phase 21 UI decision. Revisit if reasons become useful (would need `decision_trace` plus UI work).
- **O4.** Later phase: pull document text from Mayan for Jev consistency checks (income vs. payslip). This needs Mayan credentials in this service and breaks "engine knows only the request body". Deliberately out of scope.

## 14. Risks

| risk | mitigation |
|---|---|
| Jev outage or latency stalls assessments | timeout plus I3 fallback to MEDIUM; `JEV_ENABLED` kill switch |
| Model update silently changes outcomes | pin `JEV_MODEL`; log the model per decision; re-run the invariant suite on upgrade |
| Prompt injection via payload text | Jev only outputs signals; I2 caps their effect at MEDIUM; `text_anomaly` signal |
| Engine crash after receiving a request leaves the workflow stuck | same as the existing "no timeout on PENDING_RISK_ASSESSMENT" known gap; out of scope, noted |
| Invalid tier crashes the workflow signal | `RiskTier` enum; I1 test |

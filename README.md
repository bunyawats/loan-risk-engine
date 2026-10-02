# loan-risk-engine

Risk-assessment microservice for [loan-onboarding-poc](https://github.com/bunyawats/loan-onboarding-poc). It replaces that project's Python `mock_risk_engine` and decides a loan application's `risk_tier` (`LOW`, `MEDIUM`, `HIGH`) from:

1. deterministic features (loan-to-income, LTV, down-payment ratio),
2. Jev (Typesafe System One) signals over the application's free text,
3. versioned [ZEN Engine](https://github.com/gorules/zen) decision tables in `rules/`.

Jev only ever pushes an application toward human review. Auto-approve and auto-reject come from deterministic conditions alone.

`CLAUDE.md` holds the HTTP contract and invariants, `SPEC.md` the design, and `IMPLEMENTATION_PLAN.md` the task status.

## Run

```bash
cp .env.example .env          # JEV_ENABLED=false works offline
set -a; source .env; set +a
cargo run                      # listens on :8000
```

Or with a stub decisions sink standing in for KrakenD:

```bash
docker compose up --build        # RISK_ENGINE_HOST_PORT=18000 if 8000 is taken (the POC's Mayan uses it)
docker compose logs -f krakend   # shows each /decisions body
```

Smoke test:

```bash
curl -i -XPOST localhost:8000/assess -H 'content-type: application/json' -d '{
  "application_id":"APP-1","applicant_identifier":"a@b.c","product_type":"personal_loan",
  "amount":"60000","payload":{"purpose":"home renovation","employment_status":"full-time","monthly_income":"8000"}}'
curl -s localhost:8000/healthz
```

`/assess` always answers `202` with an empty body, after posting `{"application_id", "risk_tier"}` to `{KRAKEND_URL}/decisions`.

## Test

```bash
cargo fmt --check && cargo clippy --all-targets -- -D warnings
cargo test                     # no network; Jev and KrakenD are mocked
cargo test --test parity       # v1 against the mock's boundary table
cargo test --test rules_v2     # invariants I1–I4 (proptest)
cargo test --test contract     # /assess → /decisions
```

## Rules versions

| `RULES_VERSION` | behaviour |
|---|---|
| `v1` (default) | parity with the mock: `< 15,000` LOW, `< 100,000` MEDIUM, otherwise HIGH. Jev is not called. |
| `v2` | features plus Jev signals. With Jev disabled or unavailable, nothing is auto-approved. Thresholds are placeholders awaiting sign-off. |

Released rule files are immutable; a change means a new file and a new version.

## Configuration

See the table in `CLAUDE.md` and `.env.example`.

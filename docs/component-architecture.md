# Component architecture

Two views: where the service sits between loan-onboarding-poc and Typesafe, and how the
modules inside it depend on each other. `CLAUDE.md` holds the contract and invariants
these diagrams illustrate.

## 1. Deployment: two stacks that meet at host ports

The risk engine runs as its own container, started from this repo. It shares no Docker
network with the POC; each side reaches the other through a published host port.

```mermaid
flowchart LR
    subgraph POC["loan-onboarding-poc stack (its own docker compose)"]
        direction TB
        APP["app<br/>FastAPI BFFs"]
        WF["workers<br/>Temporal workflow + activities"]
        TEMPORAL[("Temporal")]
        ADAPTER["risk-adapter<br/>sole NATS client"]
        NATS(["NATS<br/>risk.assessment.submitted<br/>risk.assessment.decided"])
        KRAKEND["KrakenD gateway<br/>host port 8090"]
        MOCK["mock-risk-engine<br/>rollback only, profile 'mock'"]
    end

    subgraph ENGINE["loan-risk-engine (standalone container)"]
        SVC["risk-engine<br/>Axum, container port 8000<br/>host port 18000"]
        RULES[("rules/*.json<br/>ZEN decision tables")]
    end

    TYPESAFE["Typesafe System One API<br/>Jev decision model"]

    APP --> TEMPORAL
    TEMPORAL --- WF
    WF -- "POST /assessments" --> ADAPTER
    ADAPTER <--> NATS
    ADAPTER -- "POST /assess" --> KRAKEND
    KRAKEND -- "POST /assess<br/>host.docker.internal:18000" --> SVC
    SVC -- "POST /decisions<br/>host.docker.internal:8090" --> KRAKEND
    KRAKEND -- "POST /decisions" --> ADAPTER
    ADAPTER -- "signal_risk_decision(risk_tier)" --> TEMPORAL

    SVC --- RULES
    SVC -. "POST /v1/systemone<br/>rules v2/v3 with JEV_ENABLED=true" .-> TYPESAFE
    KRAKEND -. "rollback path" .-> MOCK
```

What the POC does with the answer:

| `risk_tier` | POC outcome |
|---|---|
| `LOW` | approved automatically |
| `MEDIUM` | `PENDING_UNDERWRITING` (human review; loans ≥ $50,000 escalate to a Manager) |
| `HIGH` | rejected automatically |

## 2. Inside the service

Arrows point from a module to what it depends on. `features` and `model` do no I/O. Only
`jev` talks to Typesafe and only `webhook` talks to KrakenD.

```mermaid
flowchart TB
    KIN(["KrakenD<br/>POST /assess, GET /healthz"])

    subgraph BIN["binary"]
        MAIN["main.rs<br/>load config and rules, tracing, graceful shutdown"]
    end

    subgraph LIB["library crate (src/lib.rs)"]
        ROUTER["lib.rs<br/>AppState, build_router"]
        CONFIG["config.rs<br/>env parsing, fail fast, Secret"]
        HANDLER["handler.rs<br/>orchestration, deadline, audit record"]
        MODEL["model.rs<br/>AssessRequest, Payload, RiskTier, Decision"]
        FEATURES["features.rs<br/>loan-to-income, LTV, down-payment ratio"]
        JEV["jev.rs<br/>question sets, Signals, timeout to 'unavailable'"]
        RULESMOD["rules.rs<br/>ZEN loader and evaluator, SHA-256"]
        WEBHOOK["webhook.rs<br/>/decisions client"]
    end

    FILES[("rules/risk_tier.v1 / v2 / v3 .json")]
    TYPESAFE(["Typesafe System One"])
    KOUT(["KrakenD<br/>POST /decisions"])
    LOG[/"stdout JSON log<br/>target risk_engine::decision"/]

    KIN --> ROUTER
    MAIN --> CONFIG
    MAIN --> RULESMOD
    MAIN --> ROUTER
    ROUTER --> HANDLER

    HANDLER --> MODEL
    HANDLER --> FEATURES
    HANDLER --> JEV
    HANDLER --> RULESMOD
    HANDLER --> WEBHOOK
    HANDLER --> LOG

    FEATURES --> MODEL
    JEV --> MODEL
    RULESMOD --> MODEL
    WEBHOOK --> MODEL

    RULESMOD --> FILES
    JEV -.-> TYPESAFE
    WEBHOOK --> KOUT
```

## 3. One assessment, step by step

```mermaid
sequenceDiagram
    participant K as KrakenD (POC)
    participant H as handler
    participant F as features
    participant J as jev
    participant T as Typesafe
    participant R as rules (ZEN)
    participant W as webhook

    K->>H: POST /assess (raw body)
    H->>H: AssessRequest::parse
    alt invalid input
        Note over H: decide MEDIUM, rule R-INVALID-INPUT
    else valid, inside ASSESS_DEADLINE_MS
        H->>F: compute(request)
        F-->>H: ratios (null if not computable)
        opt rules v2 or v3
            H->>J: signals(request)
            J->>T: state + questions (no identifier, no VIN)
            T-->>J: typed answers
            J-->>H: signals, or "unavailable" on any failure
        end
        H->>R: evaluate(features + signals)
        R-->>H: risk_tier, rule_id, reason
        Note over H: rules error gives MEDIUM (R-RULES-ERROR)<br/>deadline exceeded gives MEDIUM (R-DEADLINE)
    end
    H->>W: post_decision(application_id, risk_tier)
    W->>K: POST /decisions
    K-->>W: 2xx, or a failure that is only logged
    H->>H: emit audit log line
    H-->>K: 202 Accepted, empty body
```

## How the invariants map onto the components

| Invariant | Where it is enforced |
|---|---|
| I1: tier is always `LOW`, `MEDIUM` or `HIGH` | `RiskTier` enum in `model.rs`; `rules.rs` rejects any other literal; `handler.rs` falls back to `MEDIUM` |
| I2: Jev can only move `LOW` to `MEDIUM` | rule order in `rules/risk_tier.v2.json` and `v3.json`: every `HIGH` rule is deterministic and sits first, every Jev rule outputs `MEDIUM` |
| I3: no Jev means no auto-approve | `jev.rs` collapses every failure to `unavailable`; rule `M1` turns that into `MEDIUM` |
| I4: Manager path stays reachable | amounts from 50,000 up to the 100,000 cut-off stay `MEDIUM` in every rules version |
| I5: invalid input gives `MEDIUM` | `handler.rs` takes the raw body and decides before the rules run |

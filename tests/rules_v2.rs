//! Invariants I1–I4 over rules v2, with generated features and Jev signals.
//! (I5, invalid input → MEDIUM, is decided before the rules run; see tests/contract.rs.)

use std::path::Path;

use loan_risk_engine::config::RulesVersion;
use loan_risk_engine::features::Features;
use loan_risk_engine::jev::{EmploymentStability, JevOutcome, JevStatus, Signals};
use loan_risk_engine::model::{ProductType, RiskTier};
use loan_risk_engine::rules::{self, Rules};
use proptest::prelude::*;
use rust_decimal::Decimal;

/// The POC's MANAGER_ESCALATION_THRESHOLD_USD.
const MANAGER_ESCALATION_THRESHOLD_CENTS: i64 = 5_000_000;
const HIGH_AMOUNT_CUTOFF_CENTS: i64 = 10_000_000;

fn v2() -> Rules {
    Rules::load(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("rules"),
        RulesVersion::V2,
    )
    .expect("v2 rules load")
}

fn evaluate(
    rules: &Rules,
    product: ProductType,
    features: &Features,
    jev: &JevOutcome,
) -> RiskTier {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("runtime");
    runtime
        .block_on(rules.evaluate(rules::context(product, features, jev)))
        .expect("v2 must evaluate every generated input without error")
        .risk_tier
}

fn clean_signals() -> Signals {
    Signals {
        purpose_high_risk: Some(0.0),
        employment_stability: Some(EmploymentStability::Stable),
        vehicle_description_plausible: Some(1.0),
        address_plausible: Some(1.0),
        text_anomaly: Some(0.0),
    }
}

fn ok(signals: Signals) -> JevOutcome {
    JevOutcome {
        status: JevStatus::Ok,
        signals,
    }
}

fn product() -> impl Strategy<Value = ProductType> {
    prop_oneof![
        Just(ProductType::PersonalLoan),
        Just(ProductType::AutoLoan),
        Just(ProductType::Mortgage),
    ]
}

fn ratio() -> impl Strategy<Value = Option<Decimal>> {
    proptest::option::of((0i64..30_000).prop_map(|n| Decimal::new(n, 4)))
}

fn features() -> impl Strategy<Value = Features> {
    (0i64..20_000_000, ratio(), ratio(), ratio()).prop_map(|(cents, lti, dpr, ltv)| Features {
        amount: Decimal::new(cents, 2),
        loan_to_annual_income: lti,
        down_payment_ratio: dpr,
        ltv,
    })
}

fn probability() -> impl Strategy<Value = Option<f64>> {
    proptest::option::of(0.0f64..=1.0)
}

fn signals() -> impl Strategy<Value = Signals> {
    let stability = proptest::option::of(prop_oneof![
        Just(EmploymentStability::Stable),
        Just(EmploymentStability::Unstable),
        Just(EmploymentStability::Unclear),
    ]);
    (
        probability(),
        stability,
        probability(),
        probability(),
        probability(),
    )
        .prop_map(|(purpose, employment, vehicle, address, anomaly)| Signals {
            purpose_high_risk: purpose,
            employment_stability: employment,
            vehicle_description_plausible: vehicle,
            address_plausible: address,
            text_anomaly: anomaly,
        })
}

fn jev() -> impl Strategy<Value = JevOutcome> {
    (
        prop_oneof![Just(JevStatus::Ok), Just(JevStatus::Unavailable)],
        signals(),
    )
        .prop_map(|(status, signals)| JevOutcome { status, signals })
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(300))]

    /// I1: every input, including nulls and mismatched signals, yields one of the three tiers.
    #[test]
    fn i1_always_a_valid_tier(product in product(), features in features(), jev in jev()) {
        let rules = v2();
        let tier = evaluate(&rules, product, &features, &jev);
        prop_assert!(matches!(tier, RiskTier::Low | RiskTier::Medium | RiskTier::High));
    }

    /// I2: against the same features with clean signals, Jev can only turn LOW into MEDIUM.
    #[test]
    fn i2_jev_only_pushes_low_to_medium(product in product(), features in features(), signals in signals()) {
        let rules = v2();
        let baseline = evaluate(&rules, product, &features, &ok(clean_signals()));
        let actual = evaluate(&rules, product, &features, &ok(signals));
        match baseline {
            RiskTier::Low => prop_assert!(matches!(actual, RiskTier::Low | RiskTier::Medium)),
            other => prop_assert_eq!(actual, other),
        }
    }

    /// I2: HIGH comes only from the deterministic conditions.
    #[test]
    fn i2_high_iff_deterministic_condition(product in product(), features in features(), jev in jev()) {
        let rules = v2();
        let over = |value: Option<Decimal>, limit: Decimal| value.is_some_and(|v| v > limit);
        let deterministic_high = features.amount >= Decimal::new(HIGH_AMOUNT_CUTOFF_CENTS, 2)
            || (product == ProductType::Mortgage && over(features.ltv, Decimal::new(97, 2)))
            || (product == ProductType::PersonalLoan && over(features.loan_to_annual_income, Decimal::ONE));
        let tier = evaluate(&rules, product, &features, &jev);
        prop_assert_eq!(tier == RiskTier::High, deterministic_high);
    }

    /// I3: without Jev nothing is auto-approved, whatever the signals claim.
    #[test]
    fn i3_unavailable_never_low(product in product(), features in features(), signals in signals()) {
        let rules = v2();
        let jev = JevOutcome { status: JevStatus::Unavailable, signals };
        prop_assert_ne!(evaluate(&rules, product, &features, &jev), RiskTier::Low);
    }

    /// I4: the whole manager-escalation band stays MEDIUM for clean, affordable applications.
    #[test]
    fn i4_manager_band_is_medium(
        product in product(),
        cents in MANAGER_ESCALATION_THRESHOLD_CENTS..HIGH_AMOUNT_CUTOFF_CENTS,
        available in any::<bool>(),
    ) {
        let rules = v2();
        let features = Features {
            amount: Decimal::new(cents, 2),
            loan_to_annual_income: Some(Decimal::new(5, 1)),
            down_payment_ratio: Some(Decimal::new(2, 1)),
            ltv: Some(Decimal::new(8, 1)),
        };
        let jev = if available { ok(clean_signals()) } else { JevOutcome::unavailable() };
        prop_assert_eq!(evaluate(&rules, product, &features, &jev), RiskTier::Medium);
    }
}

#[test]
fn clean_small_application_is_low_and_each_jev_rule_escalates() {
    let rules = v2();
    let features = Features {
        amount: Decimal::new(5_000, 0),
        loan_to_annual_income: Some(Decimal::new(1, 1)),
        down_payment_ratio: Some(Decimal::new(2, 1)),
        ltv: Some(Decimal::new(8, 1)),
    };
    for product in [
        ProductType::PersonalLoan,
        ProductType::AutoLoan,
        ProductType::Mortgage,
    ] {
        assert_eq!(
            evaluate(&rules, product, &features, &ok(clean_signals())),
            RiskTier::Low
        );
    }

    let escalations = [
        (
            ProductType::AutoLoan,
            Signals {
                text_anomaly: Some(0.5),
                ..clean_signals()
            },
        ),
        (
            ProductType::PersonalLoan,
            Signals {
                purpose_high_risk: Some(0.6),
                ..clean_signals()
            },
        ),
        (
            ProductType::PersonalLoan,
            Signals {
                employment_stability: Some(EmploymentStability::Unclear),
                ..clean_signals()
            },
        ),
        (
            ProductType::AutoLoan,
            Signals {
                vehicle_description_plausible: Some(0.49),
                ..clean_signals()
            },
        ),
        (
            ProductType::Mortgage,
            Signals {
                address_plausible: Some(0.49),
                ..clean_signals()
            },
        ),
    ];
    for (product, signals) in escalations {
        assert_eq!(
            evaluate(&rules, product, &features, &ok(signals.clone())),
            RiskTier::Medium,
            "{product:?} {signals:?}"
        );
    }
}

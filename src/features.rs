//! Deterministic features. Pure functions, no I/O.

use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use serde::{Serialize, Serializer};

use crate::model::{AssessRequest, Payload};

/// Decimal places every ratio is rounded to.
const RATIO_DP: u32 = 4;
/// Turns `monthly_income` into annual income.
const MONTHS_PER_YEAR: Decimal = Decimal::from_parts(12, 0, 0, false, 0);

/// Ratios are `None` when they do not apply to the product or their denominator is not
/// positive.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Features {
    /// The requested loan amount, copied from the request.
    pub amount: Decimal,
    /// Personal loans only: `amount / (monthly_income * 12)`.
    #[serde(serialize_with = "ratio_as_number")]
    pub loan_to_annual_income: Option<Decimal>,
    /// Auto loans: `down_payment / (amount + down_payment)`, taking the vehicle price as the
    /// loan plus the down payment. Mortgages: `down_payment / appraised_value`.
    #[serde(serialize_with = "ratio_as_number")]
    pub down_payment_ratio: Option<Decimal>,
    /// Mortgages only: loan-to-value, `amount / appraised_value`.
    #[serde(serialize_with = "ratio_as_number")]
    pub ltv: Option<Decimal>,
}

/// The audit log shows the amount as an exact string and ratios as plain numbers.
fn ratio_as_number<S: Serializer>(
    value: &Option<Decimal>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    value.and_then(|d| d.to_f64()).serialize(serializer)
}

/// Computes the ratios that apply to the request's product. Never panics: an overflow
/// or a non-positive denominator leaves the ratio `None`.
///
/// # Examples
///
/// ```
/// use loan_risk_engine::features::compute;
/// use loan_risk_engine::model::{AssessRequest, Payload};
/// use rust_decimal::Decimal;
///
/// let req = AssessRequest {
///     application_id: "APP-1".into(),
///     applicant_identifier: "a@b.c".into(),
///     amount: Decimal::new(90_000, 0),
///     payload: Payload::Mortgage {
///         property_address: "1 Main St".into(),
///         appraised_value: Decimal::new(120_000, 0),
///         down_payment: Decimal::new(30_000, 0),
///     },
/// };
/// let features = compute(&req);
/// assert_eq!(features.ltv, Some(Decimal::new(75, 2))); // 90k / 120k
/// assert_eq!(features.down_payment_ratio, Some(Decimal::new(25, 2))); // 30k / 120k
/// assert_eq!(features.loan_to_annual_income, None); // personal loans only
/// ```
pub fn compute(req: &AssessRequest) -> Features {
    let amount = req.amount;
    let mut features = Features {
        amount,
        loan_to_annual_income: None,
        down_payment_ratio: None,
        ltv: None,
    };
    match &req.payload {
        Payload::PersonalLoan { monthly_income, .. } => {
            let annual_income = monthly_income.checked_mul(MONTHS_PER_YEAR);
            features.loan_to_annual_income = annual_income.and_then(|d| ratio(amount, d));
        }
        Payload::AutoLoan { down_payment, .. } => {
            let vehicle_price = amount.checked_add(*down_payment);
            features.down_payment_ratio = vehicle_price.and_then(|d| ratio(*down_payment, d));
        }
        Payload::Mortgage {
            appraised_value,
            down_payment,
            ..
        } => {
            features.ltv = ratio(amount, *appraised_value);
            features.down_payment_ratio = ratio(*down_payment, *appraised_value);
        }
    }
    features
}

/// `numerator / denominator` rounded to [`RATIO_DP`] places, or `None` when the
/// denominator is zero or negative or the division overflows.
fn ratio(numerator: Decimal, denominator: Decimal) -> Option<Decimal> {
    if denominator <= Decimal::ZERO {
        return None;
    }
    numerator
        .checked_div(denominator)
        .map(|r| r.round_dp(RATIO_DP))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    fn d(s: &str) -> Decimal {
        Decimal::from_str(s).unwrap()
    }

    fn request(amount: &str, payload: Payload) -> AssessRequest {
        AssessRequest {
            application_id: "APP-1".into(),
            applicant_identifier: "x".into(),
            amount: d(amount),
            payload,
        }
    }

    fn personal(amount: &str, income: &str) -> Features {
        compute(&request(
            amount,
            Payload::PersonalLoan {
                purpose: "p".into(),
                employment_status: "e".into(),
                monthly_income: d(income),
            },
        ))
    }

    fn mortgage(amount: &str, appraised: &str, down: &str) -> Features {
        compute(&request(
            amount,
            Payload::Mortgage {
                property_address: "a".into(),
                appraised_value: d(appraised),
                down_payment: d(down),
            },
        ))
    }

    #[test]
    fn personal_loan_to_annual_income() {
        let f = personal("60000", "8000");
        assert_eq!(f.loan_to_annual_income, Some(d("0.625")));
        assert_eq!(f.ltv, None);
        assert_eq!(f.down_payment_ratio, None);
    }

    #[test]
    fn personal_zero_or_negative_income_is_none() {
        assert_eq!(personal("60000", "0").loan_to_annual_income, None);
        assert_eq!(personal("60000", "-100").loan_to_annual_income, None);
    }

    #[test]
    fn auto_down_payment_ratio() {
        let f = compute(&request(
            "27000",
            Payload::AutoLoan {
                vehicle_make_model: "m".into(),
                vin: "v".into(),
                down_payment: d("3000"),
            },
        ));
        assert_eq!(f.down_payment_ratio, Some(d("0.1")));
        assert_eq!(f.ltv, None);
    }

    #[test]
    fn auto_non_positive_price_is_none() {
        let f = compute(&request(
            "0",
            Payload::AutoLoan {
                vehicle_make_model: "m".into(),
                vin: "v".into(),
                down_payment: d("0"),
            },
        ));
        assert_eq!(f.down_payment_ratio, None);
    }

    #[test]
    fn mortgage_ltv_and_down_payment_ratio() {
        let f = mortgage("90000", "120000", "30000");
        assert_eq!(f.ltv, Some(d("0.75")));
        assert_eq!(f.down_payment_ratio, Some(d("0.25")));
    }

    #[test]
    fn mortgage_zero_or_negative_appraisal_is_none() {
        for appraised in ["0", "-1"] {
            let f = mortgage("90000", appraised, "30000");
            assert_eq!(f.ltv, None);
            assert_eq!(f.down_payment_ratio, None);
        }
    }

    #[test]
    fn ratios_round_to_four_places() {
        assert_eq!(
            personal("10000", "2500").loan_to_annual_income,
            Some(d("0.3333"))
        );
    }

    #[test]
    fn extreme_values_do_not_panic() {
        let f = personal(
            "79228162514264337593543950335",
            "79228162514264337593543950335",
        );
        assert_eq!(f.loan_to_annual_income, None);
    }
}

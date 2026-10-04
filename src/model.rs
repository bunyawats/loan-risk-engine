//! Request/decision types. No I/O.

use std::str::FromStr;

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

/// `rule_id` for a body that could not be parsed (I5). The tier is MEDIUM.
pub const RULE_INVALID_INPUT: &str = "R-INVALID-INPUT";
/// `rule_id` when the rules fail or return something unexpected (I1). The tier is MEDIUM.
pub const RULE_RULES_ERROR: &str = "R-RULES-ERROR";
/// `rule_id` when the decide stage overruns `ASSESS_DEADLINE_MS`. The tier is MEDIUM.
pub const RULE_DEADLINE: &str = "R-DEADLINE";

/// The only values the POC's Temporal signal accepts. Never a free string.
///
/// # Examples
///
/// ```
/// use loan_risk_engine::model::RiskTier;
///
/// assert_eq!(serde_json::to_string(&RiskTier::Medium).unwrap(), r#""MEDIUM""#);
/// assert_eq!(RiskTier::High.as_str(), "HIGH");
/// assert!(serde_json::from_str::<RiskTier>(r#""VERY_HIGH""#).is_err());
/// ```
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum RiskTier {
    /// The POC auto-approves.
    Low,
    /// The POC sends the application to an underwriter (human review).
    Medium,
    /// The POC auto-rejects.
    High,
}

/// The `product_type` values the contract accepts. It decides the payload shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProductType {
    PersonalLoan,
    AutoLoan,
    Mortgage,
}

impl RiskTier {
    /// The contract literal (`"LOW"`, `"MEDIUM"`, `"HIGH"`), as used in logs.
    pub fn as_str(self) -> &'static str {
        match self {
            RiskTier::Low => "LOW",
            RiskTier::Medium => "MEDIUM",
            RiskTier::High => "HIGH",
        }
    }
}

impl ProductType {
    /// The contract literal (`"personal_loan"`, ...).
    pub fn as_str(self) -> &'static str {
        match self {
            ProductType::PersonalLoan => "personal_loan",
            ProductType::AutoLoan => "auto_loan",
            ProductType::Mortgage => "mortgage",
        }
    }
}

/// The product-specific `payload`, already validated. The variant also carries the
/// product type (see [`AssessRequest::product_type`]). String fields are untrusted free text.
#[derive(Debug, Clone, PartialEq)]
pub enum Payload {
    PersonalLoan {
        /// What the money is for. Sent to Jev.
        purpose: String,
        /// Free-text employment description. Sent to Jev.
        employment_status: String,
        /// Used for the loan-to-annual-income ratio.
        monthly_income: Decimal,
    },
    AutoLoan {
        /// Sent to Jev.
        vehicle_make_model: String,
        /// Vehicle identification number. Identifying, so never sent to Jev or logged.
        vin: String,
        /// Used for the down-payment ratio.
        down_payment: Decimal,
    },
    Mortgage {
        /// Sent to Jev.
        property_address: String,
        /// Denominator of LTV and of the down-payment ratio.
        appraised_value: Decimal,
        /// Used for the down-payment ratio.
        down_payment: Decimal,
    },
}

/// A fully validated `POST /assess` body.
#[derive(Debug, Clone, PartialEq)]
pub struct AssessRequest {
    /// The POC's application id. Echoed back in the webhook and in the audit log.
    pub application_id: String,
    /// Identifies the applicant (e.g. an email). PII: never sent to Jev and never logged.
    pub applicant_identifier: String,
    /// Requested loan amount, parsed exactly (never through `f64`).
    pub amount: Decimal,
    /// The product-specific fields.
    pub payload: Payload,
}

/// The outcome of one assessment.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Decision {
    /// The tier posted to `/decisions`.
    pub risk_tier: RiskTier,
    /// The decision-table row that fired (e.g. `H1`), or one of the `R-*` fallback ids.
    pub rule_id: String,
    /// A short human-readable explanation for the audit log.
    pub reason: String,
}

impl Decision {
    /// A MEDIUM decision, used for every fallback (invalid input, rules error, deadline).
    ///
    /// # Examples
    ///
    /// ```
    /// use loan_risk_engine::model::{Decision, RULE_DEADLINE, RiskTier};
    ///
    /// let decision = Decision::medium(RULE_DEADLINE, "assessment deadline exceeded");
    /// assert_eq!(decision.risk_tier, RiskTier::Medium);
    /// assert_eq!(decision.rule_id, "R-DEADLINE");
    /// ```
    pub fn medium(rule_id: &str, reason: &str) -> Self {
        Decision {
            risk_tier: RiskTier::Medium,
            rule_id: rule_id.to_owned(),
            reason: reason.to_owned(),
        }
    }
}

/// Why a body failed [`AssessRequest::parse`]. Each case becomes a MEDIUM decision
/// (`R-INVALID-INPUT`), not a 4xx.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum InvalidInput {
    /// The body is not JSON, or is JSON but not an object.
    #[error("body is not a JSON object")]
    NotJsonObject,
    /// A required field is absent or has the wrong JSON type. Holds the field name.
    #[error("missing or non-string field `{0}`")]
    MissingField(&'static str),
    /// A decimal field is present but does not parse. Holds the field name.
    #[error("field `{0}` is not a decimal")]
    BadDecimal(&'static str),
    /// `product_type` is not one of the three known products.
    #[error("unknown product_type")]
    UnknownProductType,
    /// `amount` is zero or negative. Without this check the rules would read it as a
    /// small loan and could auto-approve it.
    #[error("amount must be positive")]
    NonPositiveAmount,
}

/// First pass over the body: every field is optional and untyped, so a missing or
/// mistyped field becomes a precise [`InvalidInput`] instead of a generic serde error.
#[derive(Deserialize)]
struct RawRequest {
    application_id: Option<Value>,
    applicant_identifier: Option<Value>,
    product_type: Option<Value>,
    amount: Option<Value>,
    payload: Option<Value>,
}

impl AssessRequest {
    /// Parses and validates a raw `/assess` body. Unknown extra fields are ignored.
    ///
    /// # Examples
    ///
    /// ```
    /// use loan_risk_engine::model::{AssessRequest, InvalidInput, Payload, ProductType};
    /// use rust_decimal::Decimal;
    ///
    /// let body = br#"{
    ///     "application_id": "APP-1",
    ///     "applicant_identifier": "a@b.c",
    ///     "product_type": "personal_loan",
    ///     "amount": "60000.00",
    ///     "payload": {"purpose": "home renovation", "employment_status": "full-time", "monthly_income": "8000"}
    /// }"#;
    /// let req = AssessRequest::parse(body).unwrap();
    /// assert_eq!(req.product_type(), ProductType::PersonalLoan);
    /// assert_eq!(req.amount, Decimal::new(6_000_000, 2)); // exactly 60000.00
    /// assert!(matches!(req.payload, Payload::PersonalLoan { .. }));
    ///
    /// // A bad amount is an error that the handler turns into MEDIUM, not a 4xx.
    /// let bad = br#"{"application_id": "APP-1", "applicant_identifier": "x",
    ///     "product_type": "mortgage", "amount": "lots"}"#;
    /// assert_eq!(AssessRequest::parse(bad), Err(InvalidInput::BadDecimal("amount")));
    /// ```
    pub fn parse(body: &[u8]) -> Result<Self, InvalidInput> {
        let raw: RawRequest =
            serde_json::from_slice(body).map_err(|_| InvalidInput::NotJsonObject)?;
        let application_id = string(raw.application_id.as_ref(), "application_id")?;
        let applicant_identifier =
            string(raw.applicant_identifier.as_ref(), "applicant_identifier")?;
        let amount = decimal(raw.amount.as_ref(), "amount")?;
        if amount <= Decimal::ZERO {
            return Err(InvalidInput::NonPositiveAmount);
        }
        let product_type = string(raw.product_type.as_ref(), "product_type")?;
        let p = raw.payload.as_ref();
        let payload = match product_type.as_str() {
            "personal_loan" => Payload::PersonalLoan {
                purpose: string(field(p, "purpose"), "purpose")?,
                employment_status: string(field(p, "employment_status"), "employment_status")?,
                monthly_income: decimal(field(p, "monthly_income"), "monthly_income")?,
            },
            "auto_loan" => Payload::AutoLoan {
                vehicle_make_model: string(field(p, "vehicle_make_model"), "vehicle_make_model")?,
                vin: string(field(p, "vin"), "vin")?,
                down_payment: decimal(field(p, "down_payment"), "down_payment")?,
            },
            "mortgage" => Payload::Mortgage {
                property_address: string(field(p, "property_address"), "property_address")?,
                appraised_value: decimal(field(p, "appraised_value"), "appraised_value")?,
                down_payment: decimal(field(p, "down_payment"), "down_payment")?,
            },
            _ => return Err(InvalidInput::UnknownProductType),
        };
        Ok(AssessRequest {
            application_id,
            applicant_identifier,
            amount,
            payload,
        })
    }

    /// The product type, derived from the payload variant.
    pub fn product_type(&self) -> ProductType {
        match self.payload {
            Payload::PersonalLoan { .. } => ProductType::PersonalLoan,
            Payload::AutoLoan { .. } => ProductType::AutoLoan,
            Payload::Mortgage { .. } => ProductType::Mortgage,
        }
    }
}

/// Best effort: an invalid request can still be routed to a human if we know which
/// application it belongs to.
pub(crate) fn salvage_application_id(body: &[u8]) -> Option<String> {
    let value: Value = serde_json::from_slice(body).ok()?;
    value.get("application_id")?.as_str().map(str::to_owned)
}

/// `payload[name]`, or `None` when the payload or the field is absent.
fn field<'a>(payload: Option<&'a Value>, name: &str) -> Option<&'a Value> {
    payload?.get(name)
}

/// A required JSON string field, or [`InvalidInput::MissingField`].
fn string(value: Option<&Value>, name: &'static str) -> Result<String, InvalidInput> {
    value
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or(InvalidInput::MissingField(name))
}

/// The contract sends decimals as strings; JSON numbers are tolerated via their text form,
/// never through `f64`. That relies on serde_json's `arbitrary_precision` feature, which
/// makes a `Number` keep its original digits.
fn decimal(value: Option<&Value>, name: &'static str) -> Result<Decimal, InvalidInput> {
    let text = match value {
        Some(Value::String(s)) => s.trim().to_owned(),
        Some(Value::Number(n)) => n.to_string(),
        _ => return Err(InvalidInput::MissingField(name)),
    };
    Decimal::from_str(&text)
        .or_else(|_| Decimal::from_scientific(&text))
        .map_err(|_| InvalidInput::BadDecimal(name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn personal() -> Value {
        json!({
            "application_id": "APP-1",
            "applicant_identifier": "a@b.c",
            "product_type": "personal_loan",
            "amount": "60000.00",
            "payload": {"purpose": "home renovation", "employment_status": "full-time", "monthly_income": "8000"}
        })
    }

    fn parse(v: &Value) -> Result<AssessRequest, InvalidInput> {
        AssessRequest::parse(v.to_string().as_bytes())
    }

    #[test]
    fn parses_each_product_type() {
        let req = parse(&personal()).unwrap();
        assert_eq!(req.amount, Decimal::from_str("60000.00").unwrap());
        assert_eq!(req.product_type(), ProductType::PersonalLoan);

        let auto = json!({"application_id": "A", "applicant_identifier": "x", "product_type": "auto_loan",
            "amount": "32000", "payload": {"vehicle_make_model": "Toyota Yaris", "vin": "V", "down_payment": "3000"}});
        assert_eq!(parse(&auto).unwrap().product_type(), ProductType::AutoLoan);

        let mortgage = json!({"application_id": "A", "applicant_identifier": "x", "product_type": "mortgage",
            "amount": "90000", "payload": {"property_address": "1 Main St", "appraised_value": "120000", "down_payment": "30000"}});
        assert_eq!(
            parse(&mortgage).unwrap().product_type(),
            ProductType::Mortgage
        );
    }

    #[test]
    fn tolerates_numeric_decimals() {
        let mut v = personal();
        v["amount"] = json!(60000.5);
        assert_eq!(
            parse(&v).unwrap().amount,
            Decimal::from_str("60000.5").unwrap()
        );
    }

    #[test]
    fn numeric_decimals_keep_every_digit() {
        let body = personal()
            .to_string()
            .replace(r#""60000.00""#, "12345678901234567.89");
        assert_eq!(
            AssessRequest::parse(body.as_bytes()).unwrap().amount,
            Decimal::from_str("12345678901234567.89").unwrap()
        );
    }

    #[test]
    fn rejects_zero_and_negative_amounts() {
        for amount in ["0", "0.00", "-0.01", "-5000"] {
            let mut v = personal();
            v["amount"] = json!(amount);
            assert_eq!(parse(&v), Err(InvalidInput::NonPositiveAmount), "{amount}");
        }
        let mut v = personal();
        v["amount"] = json!("0.01");
        assert!(parse(&v).is_ok());
    }

    #[test]
    fn rejects_invalid_shapes() {
        assert_eq!(
            AssessRequest::parse(b"not json"),
            Err(InvalidInput::NotJsonObject)
        );
        assert_eq!(
            AssessRequest::parse(b"[1]"),
            Err(InvalidInput::NotJsonObject)
        );

        let mut v = personal();
        v["amount"] = json!("abc");
        assert_eq!(parse(&v), Err(InvalidInput::BadDecimal("amount")));

        let mut v = personal();
        v.as_object_mut().unwrap().remove("amount");
        assert_eq!(parse(&v), Err(InvalidInput::MissingField("amount")));

        let mut v = personal();
        v["product_type"] = json!("student_loan");
        assert_eq!(parse(&v), Err(InvalidInput::UnknownProductType));

        let mut v = personal();
        v["payload"] = json!({"purpose": "x"});
        assert_eq!(
            parse(&v),
            Err(InvalidInput::MissingField("employment_status"))
        );

        let mut v = personal();
        v.as_object_mut().unwrap().remove("payload");
        assert_eq!(parse(&v), Err(InvalidInput::MissingField("purpose")));

        let mut v = personal();
        v["payload"]["monthly_income"] = json!("lots");
        assert_eq!(parse(&v), Err(InvalidInput::BadDecimal("monthly_income")));
    }

    #[test]
    fn salvages_application_id_from_invalid_request() {
        let mut v = personal();
        v["amount"] = json!("abc");
        assert_eq!(
            salvage_application_id(v.to_string().as_bytes()),
            Some("APP-1".to_owned())
        );
        assert_eq!(salvage_application_id(b"garbage"), None);
        assert_eq!(salvage_application_id(br#"{"application_id": 7}"#), None);
    }

    #[test]
    fn risk_tier_serializes_to_contract_literals() {
        assert_eq!(serde_json::to_string(&RiskTier::Low).unwrap(), "\"LOW\"");
        assert_eq!(
            serde_json::to_string(&RiskTier::Medium).unwrap(),
            "\"MEDIUM\""
        );
        assert_eq!(serde_json::to_string(&RiskTier::High).unwrap(), "\"HIGH\"");
        assert!(serde_json::from_str::<RiskTier>("\"low\"").is_err());
    }
}

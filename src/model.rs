//! Request/decision types. No I/O.

use std::str::FromStr;

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

pub const RULE_INVALID_INPUT: &str = "R-INVALID-INPUT";
pub const RULE_RULES_ERROR: &str = "R-RULES-ERROR";
pub const RULE_DEADLINE: &str = "R-DEADLINE";

/// The only values the POC's Temporal signal accepts. Never a free string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum RiskTier {
    Low,
    Medium,
    High,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProductType {
    PersonalLoan,
    AutoLoan,
    Mortgage,
}

impl RiskTier {
    pub fn as_str(self) -> &'static str {
        match self {
            RiskTier::Low => "LOW",
            RiskTier::Medium => "MEDIUM",
            RiskTier::High => "HIGH",
        }
    }
}

impl ProductType {
    pub fn as_str(self) -> &'static str {
        match self {
            ProductType::PersonalLoan => "personal_loan",
            ProductType::AutoLoan => "auto_loan",
            ProductType::Mortgage => "mortgage",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Payload {
    PersonalLoan {
        purpose: String,
        employment_status: String,
        monthly_income: Decimal,
    },
    AutoLoan {
        vehicle_make_model: String,
        vin: String,
        down_payment: Decimal,
    },
    Mortgage {
        property_address: String,
        appraised_value: Decimal,
        down_payment: Decimal,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct AssessRequest {
    pub application_id: String,
    pub applicant_identifier: String,
    pub amount: Decimal,
    pub payload: Payload,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Decision {
    pub risk_tier: RiskTier,
    pub rule_id: String,
    pub reason: String,
}

impl Decision {
    pub fn medium(rule_id: &str, reason: &str) -> Self {
        Decision {
            risk_tier: RiskTier::Medium,
            rule_id: rule_id.to_owned(),
            reason: reason.to_owned(),
        }
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum InvalidInput {
    #[error("body is not a JSON object")]
    NotJsonObject,
    #[error("missing or non-string field `{0}`")]
    MissingField(&'static str),
    #[error("field `{0}` is not a decimal")]
    BadDecimal(&'static str),
    #[error("unknown product_type")]
    UnknownProductType,
}

#[derive(Deserialize)]
struct RawRequest {
    application_id: Option<Value>,
    applicant_identifier: Option<Value>,
    product_type: Option<Value>,
    amount: Option<Value>,
    payload: Option<Value>,
}

impl AssessRequest {
    pub fn parse(body: &[u8]) -> Result<Self, InvalidInput> {
        let raw: RawRequest =
            serde_json::from_slice(body).map_err(|_| InvalidInput::NotJsonObject)?;
        let application_id = string(raw.application_id.as_ref(), "application_id")?;
        let applicant_identifier =
            string(raw.applicant_identifier.as_ref(), "applicant_identifier")?;
        let amount = decimal(raw.amount.as_ref(), "amount")?;
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
pub fn salvage_application_id(body: &[u8]) -> Option<String> {
    let value: Value = serde_json::from_slice(body).ok()?;
    value.get("application_id")?.as_str().map(str::to_owned)
}

fn field<'a>(payload: Option<&'a Value>, name: &str) -> Option<&'a Value> {
    payload?.get(name)
}

fn string(value: Option<&Value>, name: &'static str) -> Result<String, InvalidInput> {
    value
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or(InvalidInput::MissingField(name))
}

/// The contract sends decimals as strings; JSON numbers are tolerated via their text form,
/// never through `f64`.
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

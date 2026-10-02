//! Environment configuration. Parsed once at startup; a bad value fails fast.

use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;

use thiserror::Error;

pub const DEFAULT_JEV_API_URL: &str = "https://api.typesafe.ai/v1/systemone";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RulesVersion {
    V1,
    V2,
    V3,
}

impl RulesVersion {
    pub fn as_str(self) -> &'static str {
        match self {
            RulesVersion::V1 => "v1",
            RulesVersion::V2 => "v2",
            RulesVersion::V3 => "v3",
        }
    }

    /// v1 is amount-only parity with the mock; later versions take Jev signals.
    pub fn uses_jev(self) -> bool {
        !matches!(self, RulesVersion::V1)
    }
}

/// Keeps the Typesafe key out of `Debug` output and logs.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(***)")
    }
}

#[derive(Debug, Clone)]
pub struct Config {
    pub port: u16,
    pub krakend_url: String,
    pub rules_version: RulesVersion,
    pub rules_dir: PathBuf,
    pub jev_enabled: bool,
    pub jev_api_url: String,
    pub typesafe_api_key: Option<Secret>,
    pub jev_model: String,
    pub jev_timeout: Duration,
    pub assess_deadline: Duration,
    pub simulated_delay: Duration,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ConfigError {
    #[error("{var}: invalid value {value:?}")]
    Invalid { var: &'static str, value: String },
    #[error("JEV_ENABLED=true requires TYPESAFE_API_KEY")]
    MissingApiKey,
}

impl Config {
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_lookup(|name| std::env::var(name).ok())
    }

    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<Self, ConfigError> {
        let get = |name: &str| {
            lookup(name)
                .map(|v| v.trim().to_owned())
                .filter(|v| !v.is_empty())
        };

        let rules_version = match get("RULES_VERSION").as_deref() {
            None | Some("v1") => RulesVersion::V1,
            Some("v2") => RulesVersion::V2,
            Some("v3") => RulesVersion::V3,
            Some(other) => return Err(invalid("RULES_VERSION", other)),
        };
        let jev_enabled = match get("JEV_ENABLED").as_deref() {
            None | Some("false") | Some("0") => false,
            Some("true") | Some("1") => true,
            Some(other) => return Err(invalid("JEV_ENABLED", other)),
        };
        let typesafe_api_key = get("TYPESAFE_API_KEY").map(Secret);
        if jev_enabled && typesafe_api_key.is_none() {
            return Err(ConfigError::MissingApiKey);
        }
        let simulated_delay_seconds: f64 = parse(&get, "SIMULATED_DELAY_SECONDS", 0.0)?;
        let simulated_delay =
            Duration::try_from_secs_f64(simulated_delay_seconds).map_err(|_| {
                invalid(
                    "SIMULATED_DELAY_SECONDS",
                    &simulated_delay_seconds.to_string(),
                )
            })?;

        Ok(Config {
            port: parse(&get, "PORT", 8000)?,
            krakend_url: get("KRAKEND_URL")
                .unwrap_or_else(|| "http://krakend:8080".to_owned())
                .trim_end_matches('/')
                .to_owned(),
            rules_version,
            rules_dir: PathBuf::from(get("RULES_DIR").unwrap_or_else(|| "/app/rules".to_owned())),
            jev_enabled,
            jev_api_url: get("JEV_API_URL").unwrap_or_else(|| DEFAULT_JEV_API_URL.to_owned()),
            typesafe_api_key,
            jev_model: get("JEV_MODEL").unwrap_or_else(|| "jev-1.13.0".to_owned()),
            jev_timeout: Duration::from_millis(parse(&get, "JEV_TIMEOUT_MS", 2000)?),
            assess_deadline: Duration::from_millis(parse(&get, "ASSESS_DEADLINE_MS", 4000)?),
            simulated_delay,
        })
    }
}

fn parse<T: FromStr>(
    get: &impl Fn(&str) -> Option<String>,
    var: &'static str,
    default: T,
) -> Result<T, ConfigError> {
    match get(var) {
        None => Ok(default),
        Some(value) => value.parse().map_err(|_| invalid(var, &value)),
    }
}

fn invalid(var: &'static str, value: &str) -> ConfigError {
    ConfigError::Invalid {
        var,
        value: value.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn config(vars: &[(&str, &str)]) -> Result<Config, ConfigError> {
        let map: HashMap<String, String> = vars
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        Config::from_lookup(|name| map.get(name).cloned())
    }

    #[test]
    fn defaults_match_the_documented_table() {
        let c = config(&[]).unwrap();
        assert_eq!(c.port, 8000);
        assert_eq!(c.krakend_url, "http://krakend:8080");
        assert_eq!(c.rules_version, RulesVersion::V1);
        assert_eq!(c.rules_dir, PathBuf::from("/app/rules"));
        assert!(!c.jev_enabled);
        assert_eq!(c.jev_api_url, DEFAULT_JEV_API_URL);
        assert_eq!(c.jev_model, "jev-1.13.0");
        assert_eq!(c.jev_timeout, Duration::from_millis(2000));
        assert_eq!(c.assess_deadline, Duration::from_millis(4000));
        assert_eq!(c.simulated_delay, Duration::ZERO);
    }

    #[test]
    fn rejects_bad_values() {
        assert!(matches!(
            config(&[("PORT", "abc")]),
            Err(ConfigError::Invalid { var: "PORT", .. })
        ));
        assert_eq!(
            config(&[("RULES_VERSION", "v3")]).unwrap().rules_version,
            RulesVersion::V3
        );
        assert!(matches!(
            config(&[("RULES_VERSION", "v9")]),
            Err(ConfigError::Invalid {
                var: "RULES_VERSION",
                ..
            })
        ));
        assert!(matches!(
            config(&[("SIMULATED_DELAY_SECONDS", "-1")]),
            Err(ConfigError::Invalid {
                var: "SIMULATED_DELAY_SECONDS",
                ..
            })
        ));
    }

    #[test]
    fn jev_enabled_requires_a_key() {
        assert_eq!(
            config(&[("JEV_ENABLED", "true")]).unwrap_err(),
            ConfigError::MissingApiKey
        );
        assert!(config(&[("JEV_ENABLED", "true"), ("TYPESAFE_API_KEY", "k")]).is_ok());
    }

    #[test]
    fn debug_output_hides_the_key() {
        let c = config(&[("TYPESAFE_API_KEY", "super-secret")]).unwrap();
        assert!(!format!("{c:?}").contains("super-secret"));
    }
}

//! Environment configuration. Parsed once at startup; a bad value fails fast.

use std::fmt;
use std::path::PathBuf;
use std::str::FromStr;
use std::time::Duration;

use thiserror::Error;

/// Typesafe System One endpoint used when `JEV_API_URL` is unset.
pub const DEFAULT_JEV_API_URL: &str = "https://api.typesafe.ai/v1/systemone";

/// Which decision table to load (`RULES_VERSION`). Each maps to
/// `rules/risk_tier.<version>.json`; released files are immutable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RulesVersion {
    /// Amount-only parity with the POC's mock: `<15k` LOW, `<100k` MEDIUM, else HIGH.
    V1,
    /// Features plus Jev signals.
    V2,
    /// v2 plus rules F1/F2: a missing loan-to-income (personal) or LTV (mortgage) is MEDIUM.
    V3,
}

impl RulesVersion {
    /// The `RULES_VERSION` literal (`"v1"`, ...), also used in the rules file name.
    pub fn as_str(self) -> &'static str {
        match self {
            RulesVersion::V1 => "v1",
            RulesVersion::V2 => "v2",
            RulesVersion::V3 => "v3",
        }
    }

    /// v1 is amount-only parity with the mock; later versions take Jev signals.
    ///
    /// # Examples
    ///
    /// ```
    /// use loan_risk_engine::config::RulesVersion;
    ///
    /// assert!(!RulesVersion::V1.uses_jev());
    /// assert!(RulesVersion::V2.uses_jev());
    /// assert!(RulesVersion::V3.uses_jev());
    /// ```
    pub fn uses_jev(self) -> bool {
        !matches!(self, RulesVersion::V1)
    }
}

/// Keeps the Typesafe key out of `Debug` output and logs.
///
/// # Examples
///
/// ```
/// use loan_risk_engine::config::Config;
///
/// let config =
///     Config::from_lookup(|name| (name == "TYPESAFE_API_KEY").then(|| "sk-123".to_owned()))
///         .unwrap();
/// let key = config.typesafe_api_key.unwrap();
/// assert_eq!(format!("{key:?}"), "Secret(***)");
/// assert_eq!(key.expose(), "sk-123");
/// ```
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    /// The raw value. Call it only where the key is sent (the Jev `Authorization` header).
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret(***)")
    }
}

/// All runtime settings. Defaults match the table in CLAUDE.md.
#[derive(Debug, Clone)]
pub struct Config {
    /// `PORT` (default `8000`). Part of the contract.
    pub port: u16,
    /// `KRAKEND_URL` (default `http://krakend:8080`), trailing `/` removed. The webhook
    /// posts to `{krakend_url}/decisions`.
    pub krakend_url: String,
    /// `RULES_VERSION` (default `v1`).
    pub rules_version: RulesVersion,
    /// `RULES_DIR` (default `/app/rules`): the folder holding `risk_tier.v*.json`.
    pub rules_dir: PathBuf,
    /// `JEV_ENABLED` (default `false`): the Jev kill switch. Accepts `true`/`1`/`false`/`0`.
    pub jev_enabled: bool,
    /// `JEV_API_URL` (default [`DEFAULT_JEV_API_URL`]).
    pub jev_api_url: String,
    /// `TYPESAFE_API_KEY`. Required when `jev_enabled`; never logged.
    pub typesafe_api_key: Option<Secret>,
    /// `JEV_MODEL` (default `jev-1.13.0`): the pinned exact model version.
    pub jev_model: String,
    /// `JEV_TIMEOUT_MS` (default `2000`): the timeout of the single Jev attempt.
    pub jev_timeout: Duration,
    /// `ASSESS_DEADLINE_MS` (default `4000`): the budget for the decide stage. Past it
    /// the decision is MEDIUM (`R-DEADLINE`). Must stay under the adapter's 5s timeout.
    pub assess_deadline: Duration,
    /// `SIMULATED_DELAY_SECONDS` (default `0`, fractions allowed): demo sleep before
    /// deciding. Counts against `assess_deadline`.
    pub simulated_delay: Duration,
}

/// Why startup refused the environment.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ConfigError {
    /// `var` is set to a value that does not parse (or is out of range).
    #[error("{var}: invalid value {value:?}")]
    Invalid { var: &'static str, value: String },
    /// `JEV_ENABLED=true` without a `TYPESAFE_API_KEY`.
    #[error("JEV_ENABLED=true requires TYPESAFE_API_KEY")]
    MissingApiKey,
}

impl Config {
    /// Reads the process environment.
    pub fn from_env() -> Result<Self, ConfigError> {
        Self::from_lookup(|name| std::env::var(name).ok())
    }

    /// Builds a config from any name→value lookup, so tests need not touch the real
    /// environment. Values are trimmed, and an empty value counts as unset.
    ///
    /// # Examples
    ///
    /// ```
    /// use std::time::Duration;
    ///
    /// use loan_risk_engine::config::{Config, ConfigError, RulesVersion};
    ///
    /// let env = |name: &str| match name {
    ///     "RULES_VERSION" => Some("v2".to_owned()),
    ///     "ASSESS_DEADLINE_MS" => Some("3000".to_owned()),
    ///     _ => None,
    /// };
    /// let config = Config::from_lookup(env).unwrap();
    /// assert_eq!(config.rules_version, RulesVersion::V2);
    /// assert_eq!(config.assess_deadline, Duration::from_millis(3000));
    /// assert_eq!(config.port, 8000); // unset, so the default
    ///
    /// // Jev on without a key is refused at startup.
    /// let env = |name: &str| (name == "JEV_ENABLED").then(|| "true".to_owned());
    /// assert_eq!(Config::from_lookup(env).unwrap_err(), ConfigError::MissingApiKey);
    /// ```
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

/// Parses `var` with `FromStr`, or returns `default` when it is unset.
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

/// Shorthand for [`ConfigError::Invalid`].
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

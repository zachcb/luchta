//! `sessions` block of the luchta config: the ports `luchta session` allocates
//! per worktree. Semantic validation lives in `luchta-sessions` (`PortPlan`).

use indexmap::IndexMap;
use serde::Deserialize;

/// Ports that `luchta session` gives each concurrent worktree its own copy of.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct SessionsConfig {
    /// Offset between one slot's ports and the next slot's.
    #[serde(
        default = "default_slot_stride",
        rename = "slotStride",
        alias = "slot_stride"
    )]
    pub slot_stride: u32,
    /// Number of slots (concurrent sessions) available.
    #[serde(
        default = "default_max_slots",
        rename = "maxSlots",
        alias = "max_slots"
    )]
    pub max_slots: u32,
    /// Env var name → port declaration, in declared order.
    #[serde(default)]
    pub ports: IndexMap<String, SessionPortSpec>,
    /// Extra env vars whose values are templates filled from the allocated
    /// ports and session identity, in declared order.
    #[serde(default)]
    pub env: IndexMap<String, String>,
}

/// One port the app reads from an environment variable.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct SessionPortSpec {
    /// Port used by slot 0.
    pub default: u16,
    /// Display and routing name (a DNS label).
    #[serde(default)]
    pub service: Option<String>,
    /// Whether the port serves HTTP that a browser can open.
    #[serde(default)]
    pub http: bool,
    /// Whether this is the service a bare `<session>.localhost` routes to.
    #[serde(default, rename = "defaultService", alias = "default_service")]
    pub default_service: bool,
}

fn default_slot_stride() -> u32 {
    1000
}

fn default_max_slots() -> u32 {
    20
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LuchtaConfig;

    #[test]
    fn parses_camel_case_with_defaults_in_declared_order() {
        let config: SessionsConfig = serde_json::from_str(
            r#"{"ports":{
                "Z_PORT":{"default":8090},
                "A_PORT":{"default":8081,"service":"web","http":true,"defaultService":true}
            }}"#,
        )
        .unwrap();

        assert_eq!(config.slot_stride, 1000);
        assert_eq!(config.max_slots, 20);
        assert_eq!(
            config.ports.keys().map(String::as_str).collect::<Vec<_>>(),
            ["Z_PORT", "A_PORT"]
        );
        assert_eq!(
            config.ports["A_PORT"],
            SessionPortSpec {
                default: 8081,
                service: Some("web".to_string()),
                http: true,
                default_service: true,
            }
        );
        let api = &config.ports["Z_PORT"];
        assert!(!api.http && api.service.is_none() && !api.default_service);
    }

    #[test]
    fn accepts_snake_case_aliases() {
        let config: SessionsConfig = serde_json::from_str(
            r#"{"slot_stride":500,"max_slots":4,
                "ports":{"P":{"default":1,"http":true,"default_service":true}}}"#,
        )
        .unwrap();

        assert_eq!(config.slot_stride, 500);
        assert_eq!(config.max_slots, 4);
        assert!(config.ports["P"].default_service);
    }

    #[test]
    fn rejects_a_default_port_above_65535() {
        let result: Result<SessionsConfig, _> =
            serde_json::from_str(r#"{"ports":{"P":{"default":70000}}}"#);
        assert!(result.is_err());
    }

    #[test]
    fn env_templates_parse_in_declared_order() {
        let config: SessionsConfig = serde_json::from_str(
            r#"{"ports":{"P":{"default":1}},"env":{
                "SESSION_LABEL":"${LUCHTA_SESSION_NAME}-${LUCHTA_SESSION_SLOT}",
                "API_ROOT_URL":"http://localhost:${P}"
            }}"#,
        )
        .unwrap();

        assert_eq!(
            config.env.keys().map(String::as_str).collect::<Vec<_>>(),
            ["SESSION_LABEL", "API_ROOT_URL"]
        );
        assert_eq!(
            config.env["API_ROOT_URL"],
            "http://localhost:${P}".to_string()
        );
    }

    #[test]
    fn env_templates_default_to_empty() {
        let config: SessionsConfig =
            serde_json::from_str(r#"{"ports":{"P":{"default":1}}}"#).unwrap();
        assert!(config.env.is_empty());
    }

    #[test]
    fn luchta_config_sessions_block_is_optional() {
        let without: LuchtaConfig = serde_json::from_str("{}").unwrap();
        assert!(without.sessions.is_none());

        let with: LuchtaConfig =
            serde_json::from_str(r#"{"sessions":{"ports":{"P":{"default":1}}}}"#).unwrap();
        assert_eq!(with.sessions.unwrap().ports["P"].default, 1);
    }
}

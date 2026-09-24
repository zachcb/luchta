//! Validated port plan: which env vars get which port in each slot.

use std::collections::BTreeSet;

use luchta_types::SessionsConfig;
use serde::{Deserialize, Serialize};

use crate::name::is_dns_label;

/// One port as handed to a session's child process.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedPort {
    pub env: String,
    pub port: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service: Option<String>,
    pub http: bool,
    #[serde(default)]
    pub default_service: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PlanError {
    #[error("sessions.ports must declare at least one port")]
    NoPorts,
    #[error("sessions.slotStride must be greater than 0")]
    ZeroStride,
    #[error("sessions.maxSlots must be greater than 0")]
    ZeroMaxSlots,
    #[error(
        "sessions.slotStride ({stride}) must be greater than the spread of the declared default ports ({spread}), otherwise slots overlap"
    )]
    StrideTooSmall { stride: u32, spread: u32 },
    #[error(
        "sessions.ports.{env}: slot {slot} would use port {port}, above 65535; lower sessions.maxSlots or sessions.slotStride"
    )]
    PortOutOfRange { env: String, slot: u32, port: u64 },
    #[error("sessions.ports: `{env}` is not a valid environment variable name")]
    InvalidEnvName { env: String },
    #[error(
        "sessions.ports.{env}.service: `{service}` must be a lowercase DNS label ([a-z0-9-], no leading or trailing hyphen, at most 63 characters)"
    )]
    InvalidServiceName { env: String, service: String },
    #[error("sessions.ports: service `{service}` is declared more than once")]
    DuplicateService { service: String },
    #[error("sessions.ports: at most one port may set defaultService")]
    MultipleDefaultServices,
    #[error("sessions.ports.{env}: defaultService requires http: true")]
    DefaultServiceNotHttp { env: String },
    #[error("sessions.ports.{env}.default must be greater than 0")]
    ZeroPort { env: String },
}

/// A validated `sessions` config. Construct with [`PortPlan::from_config`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortPlan {
    stride: u32,
    max_slots: u32,
    /// Declared ports as resolved for slot 0.
    base: Vec<ResolvedPort>,
}

impl PortPlan {
    pub fn from_config(config: &SessionsConfig) -> Result<Self, PlanError> {
        validate_limits(config)?;
        validate_names(config)?;
        validate_ranges(config)?;
        let base = config
            .ports
            .iter()
            .map(|(env, spec)| ResolvedPort {
                env: env.clone(),
                port: spec.default,
                service: spec.service.clone(),
                http: spec.http,
                default_service: spec.default_service,
            })
            .collect();
        Ok(Self {
            stride: config.slot_stride,
            max_slots: config.max_slots,
            base,
        })
    }

    pub fn max_slots(&self) -> u32 {
        self.max_slots
    }

    /// Ports for `slot`. `slot` must be below [`PortPlan::max_slots`]; the
    /// range check in `from_config` guarantees those ports fit in a `u16`.
    pub fn ports_for_slot(&self, slot: u32) -> Vec<ResolvedPort> {
        debug_assert!(slot < self.max_slots, "slot {slot} out of range");
        let offset = slot * self.stride;
        self.base
            .iter()
            .map(|base| ResolvedPort {
                port: u16::try_from(u32::from(base.port) + offset)
                    .expect("slot below max_slots stays within the validated port range"),
                ..base.clone()
            })
            .collect()
    }
}

fn validate_limits(config: &SessionsConfig) -> Result<(), PlanError> {
    if config.ports.is_empty() {
        return Err(PlanError::NoPorts);
    }
    if config.slot_stride == 0 {
        return Err(PlanError::ZeroStride);
    }
    if config.max_slots == 0 {
        return Err(PlanError::ZeroMaxSlots);
    }
    Ok(())
}

fn validate_names(config: &SessionsConfig) -> Result<(), PlanError> {
    let mut services = BTreeSet::new();
    let mut default_services = 0;
    for (env, spec) in &config.ports {
        if env.is_empty() || env.contains(['=', '\0']) {
            return Err(PlanError::InvalidEnvName { env: env.clone() });
        }
        if spec.default == 0 {
            return Err(PlanError::ZeroPort { env: env.clone() });
        }
        if let Some(service) = &spec.service {
            validate_service(env, service, &mut services)?;
        }
        if spec.default_service {
            if !spec.http {
                return Err(PlanError::DefaultServiceNotHttp { env: env.clone() });
            }
            default_services += 1;
        }
    }
    if default_services > 1 {
        return Err(PlanError::MultipleDefaultServices);
    }
    Ok(())
}

/// Checks one port's `service` name: must be a DNS label and not already
/// declared by another port in this config.
fn validate_service<'a>(
    env: &str,
    service: &'a str,
    seen: &mut BTreeSet<&'a str>,
) -> Result<(), PlanError> {
    if !is_dns_label(service) {
        return Err(PlanError::InvalidServiceName {
            env: env.to_string(),
            service: service.to_string(),
        });
    }
    if !seen.insert(service) {
        return Err(PlanError::DuplicateService {
            service: service.to_string(),
        });
    }
    Ok(())
}

fn validate_ranges(config: &SessionsConfig) -> Result<(), PlanError> {
    let defaults = config.ports.values().map(|spec| u32::from(spec.default));
    let min = defaults.clone().min().unwrap_or(0);
    let max = defaults.max().unwrap_or(0);
    let spread = max - min;
    if config.slot_stride <= spread {
        return Err(PlanError::StrideTooSmall {
            stride: config.slot_stride,
            spread,
        });
    }
    let last = config.max_slots - 1;
    for (env, spec) in &config.ports {
        let port = u64::from(spec.default) + u64::from(last) * u64::from(config.slot_stride);
        if port > u64::from(u16::MAX) {
            return Err(PlanError::PortOutOfRange {
                env: env.clone(),
                slot: last,
                port,
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use luchta_types::SessionsConfig;

    fn config(json: &str) -> SessionsConfig {
        serde_json::from_str(json).unwrap()
    }

    fn plan(json: &str) -> Result<PortPlan, PlanError> {
        PortPlan::from_config(&config(json))
    }

    const TWO_PORTS: &str = r#"{"ports":{
        "WEB":{"default":8081,"service":"web","http":true},
        "API":{"default":8090}
    }}"#;

    #[test]
    fn resolves_ports_for_a_slot_in_declared_order() {
        let plan = plan(TWO_PORTS).unwrap();
        let slot1 = plan.ports_for_slot(1);
        assert_eq!(
            slot1
                .iter()
                .map(|p| (p.env.as_str(), p.port))
                .collect::<Vec<_>>(),
            [("WEB", 9081), ("API", 9090)]
        );
        assert_eq!(slot1[0].service.as_deref(), Some("web"));
        assert!(slot1[0].http);
        assert!(!slot1[1].http);
        assert_eq!(plan.max_slots(), 20);
    }

    #[test]
    fn slot_zero_uses_the_defaults() {
        let ports = plan(TWO_PORTS).unwrap().ports_for_slot(0);
        assert_eq!(ports[0].port, 8081);
        assert_eq!(ports[1].port, 8090);
    }

    #[test]
    fn rejects_a_stride_that_does_not_exceed_the_port_spread() {
        let json = r#"{"slotStride":9,"ports":{"A":{"default":8081},"B":{"default":8090}}}"#;
        assert_eq!(
            plan(json),
            Err(PlanError::StrideTooSmall {
                stride: 9,
                spread: 9
            })
        );
    }

    #[test]
    fn accepts_a_stride_just_above_the_spread() {
        let json = r#"{"slotStride":10,"ports":{"A":{"default":8081},"B":{"default":8090}}}"#;
        assert!(plan(json).is_ok());
    }

    #[test]
    fn rejects_ports_past_65535_in_the_last_slot() {
        let json = r#"{"ports":{"WEB":{"default":60000}}}"#;
        assert_eq!(
            plan(json),
            Err(PlanError::PortOutOfRange {
                env: "WEB".to_string(),
                slot: 19,
                port: 79000
            })
        );
    }

    #[test]
    fn rejects_empty_ports_and_zero_limits() {
        assert_eq!(plan(r#"{"ports":{}}"#), Err(PlanError::NoPorts));
        assert_eq!(
            plan(r#"{"slotStride":0,"ports":{"A":{"default":1}}}"#),
            Err(PlanError::ZeroStride)
        );
        assert_eq!(
            plan(r#"{"maxSlots":0,"ports":{"A":{"default":1}}}"#),
            Err(PlanError::ZeroMaxSlots)
        );
    }

    #[test]
    fn rejects_bad_env_and_service_names() {
        assert_eq!(
            plan(r#"{"ports":{"A=B":{"default":1}}}"#),
            Err(PlanError::InvalidEnvName {
                env: "A=B".to_string()
            })
        );
        assert_eq!(
            plan(r#"{"ports":{"A":{"default":1,"service":"Web_1"}}}"#),
            Err(PlanError::InvalidServiceName {
                env: "A".to_string(),
                service: "Web_1".to_string()
            })
        );
    }

    #[test]
    fn rejects_duplicate_services_and_multiple_defaults() {
        assert_eq!(
            plan(
                r#"{"ports":{"A":{"default":1,"service":"web"},"B":{"default":2,"service":"web"}}}"#
            ),
            Err(PlanError::DuplicateService {
                service: "web".to_string()
            })
        );
        assert_eq!(
            plan(
                r#"{"ports":{
                "A":{"default":1,"http":true,"defaultService":true},
                "B":{"default":2,"http":true,"defaultService":true}}}"#
            ),
            Err(PlanError::MultipleDefaultServices)
        );
    }

    #[test]
    fn rejects_a_default_service_that_is_not_http() {
        assert_eq!(
            plan(r#"{"ports":{"A":{"default":1,"defaultService":true}}}"#),
            Err(PlanError::DefaultServiceNotHttp {
                env: "A".to_string()
            })
        );
    }

    #[test]
    fn rejects_a_zero_default_port() {
        assert_eq!(
            plan(r#"{"ports":{"A":{"default":0}}}"#),
            Err(PlanError::ZeroPort {
                env: "A".to_string()
            })
        );
    }
}

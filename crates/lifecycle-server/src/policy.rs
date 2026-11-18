//! The trust boundary the lifecycle server enforces: where the sandbox
//! lives, what it may reach, and how much of the host it may take.
//!
//! Every rule here is PURE: the decision is taken before anything is
//! created, and a backend that cannot honour it is refused instead of
//! degraded.

use std::net::IpAddr;

use crate::compose::{ValidationError, validation_error};
use crate::model::error_codes;

/// The only network policy the server implements: the sandbox lives in
/// an INTERNAL Docker network and reaches the internet through the
/// authorized egress proxy, never directly.
pub const INTERNAL_EGRESS_PROXY: &str = "internal-egress-proxy";

/// The network the sandboxes share.
pub const DEFAULT_SANDBOX_NETWORK: &str = "hodei-mh-sandboxes";

/// The subnet of the isolated network.
pub const DEFAULT_SANDBOX_SUBNET: &str = "172.30.0.0/16";

/// The address the published sandbox ports are bound to. The loopback:
/// a published port is reachable from the host, and from nowhere else.
pub const DEFAULT_PUBLISH_ADDRESS: &str = "127.0.0.1";

/// The network policy a sandbox runs under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetworkPolicy {
    /// An isolated internal Docker network with IPv6 disabled. An egress
    /// proxy, when configured, must be attached separately.
    InternalEgressProxy {
        network_name: String,
        subnet: String,
    },
}

impl NetworkPolicy {
    /// Build the policy from its configured name.
    ///
    /// An unknown policy is REFUSED: the server does not fall back to a
    /// policy it was not asked for, and it never falls back to the
    /// default bridge, which would hand the sandbox the whole host's
    /// network.
    pub fn parse(policy: &str, network_name: &str, subnet: &str) -> Result<Self, ValidationError> {
        match policy {
            INTERNAL_EGRESS_PROXY => {
                gateway_of(subnet)?;
                Ok(Self::InternalEgressProxy {
                    network_name: network_name.to_string(),
                    subnet: subnet.to_string(),
                })
            }
            other => Err(validation_error(
                error_codes::INVALID_PARAMETER,
                format!(
                    "unknown network policy {other:?}; the supported policy is {INTERNAL_EGRESS_PROXY:?}"
                ),
            )),
        }
    }

    pub fn network_name(&self) -> &str {
        match self {
            Self::InternalEgressProxy { network_name, .. } => network_name,
        }
    }

    pub fn subnet(&self) -> &str {
        match self {
            Self::InternalEgressProxy { subnet, .. } => subnet,
        }
    }
}

/// The first usable address of an IPv4 subnet.
pub fn gateway_of(subnet: &str) -> Result<String, ValidationError> {
    let (address, prefix) = subnet.split_once('/').ok_or_else(|| {
        validation_error(
            error_codes::INVALID_PARAMETER,
            format!("the network subnet {subnet:?} is not a CIDR block"),
        )
    })?;
    let prefix: u32 = prefix.parse().map_err(|_| {
        validation_error(
            error_codes::INVALID_PARAMETER,
            format!("the network subnet {subnet:?} has a malformed prefix"),
        )
    })?;
    if prefix > 30 {
        return Err(validation_error(
            error_codes::INVALID_PARAMETER,
            format!("the network subnet {subnet:?} has an impossible prefix"),
        ));
    }
    let octets: Vec<u8> = address
        .split('.')
        .map(|octet| {
            octet.parse::<u8>().map_err(|_| {
                validation_error(
                    error_codes::INVALID_PARAMETER,
                    format!("the network subnet {subnet:?} has a malformed address"),
                )
            })
        })
        .collect::<Result<Vec<u8>, ValidationError>>()?;
    if octets.len() != 4 {
        return Err(validation_error(
            error_codes::INVALID_PARAMETER,
            format!("the network subnet {subnet:?} is not an IPv4 block"),
        ));
    }
    let network = u32::from_be_bytes([octets[0], octets[1], octets[2], octets[3]]);
    let mask = u32::MAX.checked_shl(32 - prefix).unwrap_or(0);
    if network & !mask != 0 {
        return Err(validation_error(
            error_codes::INVALID_PARAMETER,
            format!("the network subnet {subnet:?} has host bits set"),
        ));
    }
    let gateway = network + 1;
    let bytes = gateway.to_be_bytes();
    Ok(format!(
        "{}.{}.{}.{}",
        bytes[0], bytes[1], bytes[2], bytes[3]
    ))
}

/// The limits every sandbox runs under. They are MANDATORY: an
/// environment without them has no boundary, so a configuration that
/// lacks them never starts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LimitPolicy {
    pub memory_bytes: i64,
    pub nano_cpus: i64,
    pub pids_limit: i64,
}

impl LimitPolicy {
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.memory_bytes <= 0 || self.nano_cpus <= 0 || self.pids_limit <= 0 {
            return Err(validation_error(
                error_codes::INVALID_PARAMETER,
                "memory, CPU and PID limits must be positive".to_string(),
            ));
        }
        Ok(())
    }
}

impl Default for LimitPolicy {
    fn default() -> Self {
        Self {
            memory_bytes: 4 * 1024 * 1024 * 1024,
            nano_cpus: 2_000_000_000,
            pids_limit: 4096,
        }
    }
}

/// The effective limits of a sandbox: the request may ask for LESS than
/// the policy allows, never for more.
///
/// A request that exceeds the policy is REFUSED rather than silently
/// capped: a caller that asked for more must learn that it did not get
/// it, and it must learn it BEFORE the environment exists.
pub fn effective_limits(
    requested_memory: Option<i64>,
    requested_nano_cpus: Option<i64>,
    policy: &LimitPolicy,
) -> Result<(i64, i64), ValidationError> {
    policy.validate()?;
    let memory = requested_memory.unwrap_or(policy.memory_bytes);
    if memory <= 0 {
        return Err(validation_error(
            error_codes::INVALID_PARAMETER,
            "memory limit must be positive",
        ));
    }
    if memory > policy.memory_bytes {
        return Err(validation_error(
            error_codes::INVALID_PARAMETER,
            format!(
                "the requested memory limit {memory} bytes exceeds the maximum of {} bytes",
                policy.memory_bytes
            ),
        ));
    }
    let nano_cpus = requested_nano_cpus.unwrap_or(policy.nano_cpus);
    if nano_cpus <= 0 {
        return Err(validation_error(
            error_codes::INVALID_PARAMETER,
            "CPU limit must be positive",
        ));
    }
    if nano_cpus > policy.nano_cpus {
        return Err(validation_error(
            error_codes::INVALID_PARAMETER,
            format!(
                "the requested cpu limit {nano_cpus} exceeds the maximum of {}",
                policy.nano_cpus
            ),
        ));
    }
    Ok((memory, nano_cpus))
}

/// The address the sandbox ports are published on.
///
/// A non-loopback address exposes every sandbox to the network the
/// host sits in, so it requires an EXPLICIT acceptance: the operator
/// says so, in the environment, with the reason spelled out.
pub fn publish_address(
    raw: &str,
    public_publish_accepted: bool,
) -> Result<IpAddr, ValidationError> {
    let address: IpAddr = raw.parse().map_err(|_| {
        validation_error(
            error_codes::INVALID_PARAMETER,
            format!("the publish address {raw:?} is not an IP address"),
        )
    })?;
    if !address.is_loopback() && !public_publish_accepted {
        return Err(validation_error(
            error_codes::INVALID_PARAMETER,
            format!(
                "publishing the sandbox ports on {address} exposes every sandbox to the network \
                 the host sits in; set HODEI_MH_ALLOW_PUBLIC_PUBLISH=1 to accept that"
            ),
        ));
    }
    Ok(address)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_only_supported_policy_is_the_internal_egress_proxy() {
        let policy = NetworkPolicy::parse(
            INTERNAL_EGRESS_PROXY,
            DEFAULT_SANDBOX_NETWORK,
            DEFAULT_SANDBOX_SUBNET,
        )
        .expect("the supported policy");
        assert_eq!(policy.network_name(), "hodei-mh-sandboxes");
        assert_eq!(policy.subnet(), "172.30.0.0/16");
    }

    #[test]
    fn an_unknown_policy_is_refused_instead_of_falling_back() {
        let error = NetworkPolicy::parse("bridge", DEFAULT_SANDBOX_NETWORK, DEFAULT_SANDBOX_SUBNET)
            .expect_err("an unknown policy is refused");
        assert_eq!(error.code, error_codes::INVALID_PARAMETER);
        assert!(
            error.message.contains("unknown network policy"),
            "{error:?}"
        );
    }

    #[test]
    fn the_gateway_is_the_first_address_of_the_subnet() {
        assert_eq!(gateway_of("172.30.0.0/16").unwrap(), "172.30.0.1");
        assert_eq!(gateway_of("10.1.2.0/24").unwrap(), "10.1.2.1");
        assert!(gateway_of("172.30.0.0").is_err());
        assert!(gateway_of("172.30.0.0/abc").is_err());
        assert!(gateway_of("172.30.0/16").is_err());
        assert!(gateway_of("172.30.0.0/64").is_err());
        assert!(gateway_of("172.30.1.0/16").is_err());
        assert!(gateway_of("255.255.255.255/32").is_err());
        assert!(gateway_of("10.0.0.0/31").is_err());
    }

    #[test]
    fn the_limits_of_a_request_are_the_policy_when_it_asks_for_no_more() {
        let policy = LimitPolicy::default();
        let (memory, cpus) = effective_limits(None, None, &policy).unwrap();
        assert_eq!(memory, policy.memory_bytes);
        assert_eq!(cpus, policy.nano_cpus);
    }

    #[test]
    fn non_positive_mandatory_limits_are_refused() {
        let policy = LimitPolicy {
            memory_bytes: 0,
            ..LimitPolicy::default()
        };
        assert!(effective_limits(None, None, &policy).is_err());
        let policy = LimitPolicy {
            nano_cpus: -1,
            ..LimitPolicy::default()
        };
        assert!(effective_limits(None, None, &policy).is_err());
        let policy = LimitPolicy {
            pids_limit: 0,
            ..LimitPolicy::default()
        };
        assert!(effective_limits(None, None, &policy).is_err());
    }

    #[test]
    fn a_request_may_ask_for_less_but_never_for_more() {
        let policy = LimitPolicy::default();
        assert!(effective_limits(Some(1024), Some(1_000_000), &policy).is_ok());
        let too_much_memory = effective_limits(Some(policy.memory_bytes + 1), None, &policy)
            .expect_err("more memory than the policy allows");
        assert!(too_much_memory.message.contains("exceeds the maximum"));
        let too_many_cpus =
            effective_limits(None, Some(policy.nano_cpus + 1), &policy).expect_err("too many cpus");
        assert!(too_many_cpus.message.contains("exceeds the maximum"));
    }

    #[test]
    fn a_non_loopback_publish_address_needs_an_explicit_acceptance() {
        assert_eq!(
            publish_address("127.0.0.1", false).unwrap(),
            "127.0.0.1".parse::<IpAddr>().unwrap()
        );
        assert!(publish_address("0.0.0.0", false).is_err());
        assert!(publish_address("0.0.0.0", true).is_ok());
        assert!(publish_address("not-an-address", true).is_err());
    }
}

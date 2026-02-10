use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use url::Url;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum NetworkPolicyMode {
    AllowAll,
    AllowList,
    DenyList,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkPolicy {
    pub mode: NetworkPolicyMode,
    pub allowed_domains: Vec<String>,
    pub denied_domains: Vec<String>,
    pub block_private_ips: bool,
    pub block_metadata_endpoints: bool,
    pub block_loopback: bool,
    pub allowed_schemes: Vec<String>,
    pub allowed_ports: Vec<u16>,
    pub max_redirects: usize,
    pub dns_rebinding_protection: bool,
}

impl Default for NetworkPolicy {
    fn default() -> Self {
        Self {
            mode: NetworkPolicyMode::AllowAll,
            allowed_domains: vec![],
            denied_domains: vec![],
            block_private_ips: true,
            block_metadata_endpoints: true,
            block_loopback: true,
            allowed_schemes: vec!["https".into(), "http".into()],
            allowed_ports: vec![80, 443, 8080, 8443],
            max_redirects: 3,
            dns_rebinding_protection: true,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum NetworkError {
    #[error("Invalid URL: {0}")]
    InvalidUrl(String),
    #[error("Blocked scheme: {0}")]
    BlockedScheme(String),
    #[error("Blocked port: {0}")]
    BlockedPort(u16),
    #[error("Domain not allowed: {0}")]
    DomainNotAllowed(String),
    #[error("Domain denied: {0}")]
    DomainDenied(String),
    #[error("DNS resolution failed: {0}")]
    DnsResolutionFailed(String),
    #[error("Blocked IP: {host} -> {ip}")]
    BlockedIp { host: String, ip: IpAddr },
    #[error("Redirect blocked: {0}")]
    RedirectBlocked(String),
    #[error("HTTP client build failed: {0}")]
    ClientBuildFailed(String),
}

pub fn is_blocked_ip(ip: &IpAddr, policy: &NetworkPolicy) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            if policy.block_loopback && v4.is_loopback() {
                return true;
            }
            if policy.block_private_ips
                && (v4.is_private() || v4.is_link_local() || v4.octets()[0] == 0)
            {
                return true;
            }
            if policy.block_metadata_endpoints {
                let octets = v4.octets();
                if octets == [169, 254, 169, 254]
                    || octets == [169, 254, 170, 2]
                    || octets == [100, 100, 100, 200]
                {
                    return true;
                }
            }
            false
        }
        IpAddr::V6(v6) => {
            if policy.block_loopback && v6.is_loopback() {
                return true;
            }
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_blocked_ip(&IpAddr::V4(v4), policy);
            }
            if policy.block_private_ips && (v6.segments()[0] & 0xffc0) == 0xfe80 {
                return true;
            }
            false
        }
    }
}

pub fn domain_matches(host: &str, pattern: &str) -> bool {
    if pattern.starts_with("*.") {
        let suffix = &pattern[1..];
        host.ends_with(suffix) || host == &pattern[2..]
    } else {
        host == pattern
    }
}

pub struct SafeDnsResolver {
    policy: NetworkPolicy,
}

impl SafeDnsResolver {
    pub fn new(policy: NetworkPolicy) -> Self {
        Self { policy }
    }

    pub async fn resolve_and_validate(&self, host: &str) -> Result<Vec<IpAddr>, NetworkError> {
        if let Ok(ip) = host.parse::<IpAddr>() {
            if is_blocked_ip(&ip, &self.policy) {
                return Err(NetworkError::BlockedIp {
                    host: host.to_string(),
                    ip,
                });
            }
            return Ok(vec![ip]);
        }

        let addrs = tokio::net::lookup_host((host, 0))
            .await
            .map_err(|_| NetworkError::DnsResolutionFailed(host.to_string()))?;

        let ips: Vec<IpAddr> = addrs.map(|a| a.ip()).collect();
        if ips.is_empty() {
            return Err(NetworkError::DnsResolutionFailed(host.to_string()));
        }

        for ip in &ips {
            if is_blocked_ip(ip, &self.policy) {
                return Err(NetworkError::BlockedIp {
                    host: host.to_string(),
                    ip: *ip,
                });
            }
        }

        Ok(ips)
    }
}

impl reqwest::dns::Resolve for SafeDnsResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let host = name.as_str().to_string();
        let policy = self.policy.clone();
        Box::pin(async move {
            let resolver = SafeDnsResolver::new(policy);
            let ips = resolver.resolve_and_validate(&host).await?;
            let addrs: Vec<SocketAddr> = ips.into_iter().map(|ip| SocketAddr::new(ip, 0)).collect();
            Ok(Box::new(addrs.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

pub async fn validate_url(url: &str, policy: &NetworkPolicy) -> Result<(), NetworkError> {
    let parsed = Url::parse(url).map_err(|_| NetworkError::InvalidUrl(url.to_string()))?;

    if !policy
        .allowed_schemes
        .iter()
        .any(|s| s.eq_ignore_ascii_case(parsed.scheme()))
    {
        return Err(NetworkError::BlockedScheme(parsed.scheme().to_string()));
    }

    let port = parsed.port_or_known_default().unwrap_or(0);
    if !policy.allowed_ports.is_empty() && !policy.allowed_ports.contains(&port) {
        return Err(NetworkError::BlockedPort(port));
    }

    let host = parsed
        .host_str()
        .ok_or_else(|| NetworkError::InvalidUrl(url.to_string()))?;

    match &policy.mode {
        NetworkPolicyMode::AllowList => {
            if !policy.allowed_domains.iter().any(|d| domain_matches(host, d)) {
                return Err(NetworkError::DomainNotAllowed(host.to_string()));
            }
        }
        NetworkPolicyMode::DenyList => {
            if policy.denied_domains.iter().any(|d| domain_matches(host, d)) {
                return Err(NetworkError::DomainDenied(host.to_string()));
            }
        }
        NetworkPolicyMode::AllowAll => {}
    }

    let resolver = SafeDnsResolver::new(policy.clone());
    resolver.resolve_and_validate(host).await?;
    Ok(())
}

fn validate_redirect_url_sync(url: &Url, policy: &NetworkPolicy) -> Result<(), NetworkError> {
    if !policy
        .allowed_schemes
        .iter()
        .any(|s| s.eq_ignore_ascii_case(url.scheme()))
    {
        return Err(NetworkError::BlockedScheme(url.scheme().to_string()));
    }

    let port = url.port_or_known_default().unwrap_or(0);
    if !policy.allowed_ports.is_empty() && !policy.allowed_ports.contains(&port) {
        return Err(NetworkError::BlockedPort(port));
    }

    let host = url
        .host_str()
        .ok_or_else(|| NetworkError::InvalidUrl(url.to_string()))?;

    match &policy.mode {
        NetworkPolicyMode::AllowList => {
            if !policy.allowed_domains.iter().any(|d| domain_matches(host, d)) {
                return Err(NetworkError::DomainNotAllowed(host.to_string()));
            }
        }
        NetworkPolicyMode::DenyList => {
            if policy.denied_domains.iter().any(|d| domain_matches(host, d)) {
                return Err(NetworkError::DomainDenied(host.to_string()));
            }
        }
        NetworkPolicyMode::AllowAll => {}
    }

    Ok(())
}

pub struct SecureHttpClientFactory;

impl SecureHttpClientFactory {
    pub fn build(policy: &NetworkPolicy, timeout: std::time::Duration) -> Result<reqwest::Client, NetworkError> {
        let resolver = SafeDnsResolver::new(policy.clone());
        let policy_clone = policy.clone();

        let redirect_policy = reqwest::redirect::Policy::custom(move |attempt| {
            if attempt.previous().len() >= policy_clone.max_redirects {
                return attempt.error(NetworkError::RedirectBlocked(
                    "max redirects exceeded".to_string(),
                ));
            }
            match validate_redirect_url_sync(attempt.url(), &policy_clone) {
                Ok(_) => attempt.follow(),
                Err(err) => attempt.error(err),
            }
        });

        let mut builder = reqwest::Client::builder()
            .redirect(redirect_policy)
            .dns_resolver(Arc::new(resolver))
            .connect_timeout(std::time::Duration::from_secs(10))
            .timeout(timeout);

        if policy.dns_rebinding_protection {
            builder = builder.pool_max_idle_per_host(0);
        }

        builder.build().map_err(|e| NetworkError::ClientBuildFailed(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_domain_matches() {
        assert!(domain_matches("api.example.com", "*.example.com"));
        assert!(domain_matches("example.com", "*.example.com"));
        assert!(domain_matches("example.com", "example.com"));
        assert!(!domain_matches("evil.com", "*.example.com"));
    }

    #[test]
    fn test_blocked_ip() {
        let policy = NetworkPolicy::default();
        let ip: IpAddr = "127.0.0.1".parse().unwrap();
        assert!(is_blocked_ip(&ip, &policy));
    }
}
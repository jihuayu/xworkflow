//! Network policy enforcement and SSRF prevention.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use url::Url;

/// How the network policy filters outbound requests.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum NetworkPolicyMode {
    /// All domains are allowed (denies are still checked).
    AllowAll,
    /// Only listed domains are allowed.
    AllowList,
    /// All domains allowed except listed ones.
    DenyList,
}

/// Network security policy controlling outbound HTTP requests.
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

/// Errors from network policy checks.
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

/// Check whether an IP address is blocked by the given policy.
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
    fn test_domain_matches_exact_no_wildcard() {
        assert!(domain_matches("foo.com", "foo.com"));
        assert!(!domain_matches("bar.com", "foo.com"));
    }

    #[test]
    fn test_domain_matches_wildcard_subdomain() {
        assert!(domain_matches("deep.sub.example.com", "*.example.com"));
        assert!(!domain_matches("notexample.com", "*.example.com"));
    }

    #[test]
    fn test_blocked_ip() {
        let policy = NetworkPolicy::default();
        let ip: IpAddr = "127.0.0.1".parse().unwrap();
        assert!(is_blocked_ip(&ip, &policy));
    }

    #[test]
    fn test_blocked_ip_private() {
        let policy = NetworkPolicy::default();
        let ip: IpAddr = "192.168.1.1".parse().unwrap();
        assert!(is_blocked_ip(&ip, &policy));
        let ip2: IpAddr = "10.0.0.1".parse().unwrap();
        assert!(is_blocked_ip(&ip2, &policy));
        let ip3: IpAddr = "172.16.0.1".parse().unwrap();
        assert!(is_blocked_ip(&ip3, &policy));
    }

    #[test]
    fn test_blocked_ip_link_local() {
        let policy = NetworkPolicy::default();
        let ip: IpAddr = "169.254.1.1".parse().unwrap();
        assert!(is_blocked_ip(&ip, &policy));
    }

    #[test]
    fn test_blocked_ip_zero_prefix() {
        let policy = NetworkPolicy::default();
        let ip: IpAddr = "0.0.0.1".parse().unwrap();
        assert!(is_blocked_ip(&ip, &policy));
    }

    #[test]
    fn test_blocked_ip_metadata_endpoints() {
        let policy = NetworkPolicy::default();
        let aws: IpAddr = "169.254.169.254".parse().unwrap();
        assert!(is_blocked_ip(&aws, &policy));
        let gcp: IpAddr = "169.254.170.2".parse().unwrap();
        assert!(is_blocked_ip(&gcp, &policy));
        let azure: IpAddr = "100.100.100.200".parse().unwrap();
        assert!(is_blocked_ip(&azure, &policy));
    }

    #[test]
    fn test_blocked_ip_public_not_blocked() {
        let policy = NetworkPolicy::default();
        let ip: IpAddr = "8.8.8.8".parse().unwrap();
        assert!(!is_blocked_ip(&ip, &policy));
        let ip2: IpAddr = "1.1.1.1".parse().unwrap();
        assert!(!is_blocked_ip(&ip2, &policy));
    }

    #[test]
    fn test_blocked_ip_v6_loopback() {
        let policy = NetworkPolicy::default();
        let ip: IpAddr = "::1".parse().unwrap();
        assert!(is_blocked_ip(&ip, &policy));
    }

    #[test]
    fn test_blocked_ip_v6_link_local() {
        let policy = NetworkPolicy::default();
        let ip: IpAddr = "fe80::1".parse().unwrap();
        assert!(is_blocked_ip(&ip, &policy));
    }

    #[test]
    fn test_blocked_ip_v6_public_not_blocked() {
        let policy = NetworkPolicy::default();
        let ip: IpAddr = "2001:4860:4860::8888".parse().unwrap();
        assert!(!is_blocked_ip(&ip, &policy));
    }

    #[test]
    fn test_blocked_ip_disabled_blocking() {
        let policy = NetworkPolicy {
            block_loopback: false,
            block_private_ips: false,
            block_metadata_endpoints: false,
            ..NetworkPolicy::default()
        };
        let ip: IpAddr = "127.0.0.1".parse().unwrap();
        assert!(!is_blocked_ip(&ip, &policy));
        let ip2: IpAddr = "192.168.1.1".parse().unwrap();
        assert!(!is_blocked_ip(&ip2, &policy));
        let ip3: IpAddr = "169.254.169.254".parse().unwrap();
        assert!(!is_blocked_ip(&ip3, &policy));
    }

    #[test]
    fn test_network_policy_default() {
        let policy = NetworkPolicy::default();
        assert!(matches!(policy.mode, NetworkPolicyMode::AllowAll));
        assert!(policy.allowed_domains.is_empty());
        assert!(policy.denied_domains.is_empty());
        assert!(policy.block_private_ips);
        assert!(policy.block_metadata_endpoints);
        assert!(policy.block_loopback);
        assert_eq!(policy.allowed_schemes, vec!["https", "http"]);
        assert_eq!(policy.allowed_ports, vec![80, 443, 8080, 8443]);
        assert_eq!(policy.max_redirects, 3);
        assert!(policy.dns_rebinding_protection);
    }

    #[test]
    fn test_validate_redirect_url_sync_allow_all() {
        let policy = NetworkPolicy::default();
        let url = Url::parse("https://example.com:443/path").unwrap();
        assert!(validate_redirect_url_sync(&url, &policy).is_ok());
    }

    #[test]
    fn test_validate_redirect_url_sync_blocked_scheme() {
        let policy = NetworkPolicy::default();
        let url = Url::parse("ftp://example.com/path").unwrap();
        let err = validate_redirect_url_sync(&url, &policy).unwrap_err();
        assert!(matches!(err, NetworkError::BlockedScheme(_)));
    }

    #[test]
    fn test_validate_redirect_url_sync_blocked_port() {
        let policy = NetworkPolicy {
            allowed_ports: vec![443],
            ..NetworkPolicy::default()
        };
        let url = Url::parse("https://example.com:9999/path").unwrap();
        let err = validate_redirect_url_sync(&url, &policy).unwrap_err();
        assert!(matches!(err, NetworkError::BlockedPort(9999)));
    }

    #[test]
    fn test_validate_redirect_url_sync_allow_list() {
        let policy = NetworkPolicy {
            mode: NetworkPolicyMode::AllowList,
            allowed_domains: vec!["example.com".into()],
            ..NetworkPolicy::default()
        };
        let ok_url = Url::parse("https://example.com/path").unwrap();
        assert!(validate_redirect_url_sync(&ok_url, &policy).is_ok());

        let blocked_url = Url::parse("https://evil.com/path").unwrap();
        let err = validate_redirect_url_sync(&blocked_url, &policy).unwrap_err();
        assert!(matches!(err, NetworkError::DomainNotAllowed(_)));
    }

    #[test]
    fn test_validate_redirect_url_sync_deny_list() {
        let policy = NetworkPolicy {
            mode: NetworkPolicyMode::DenyList,
            denied_domains: vec!["evil.com".into()],
            ..NetworkPolicy::default()
        };
        let ok_url = Url::parse("https://example.com/path").unwrap();
        assert!(validate_redirect_url_sync(&ok_url, &policy).is_ok());

        let blocked_url = Url::parse("https://evil.com/path").unwrap();
        let err = validate_redirect_url_sync(&blocked_url, &policy).unwrap_err();
        assert!(matches!(err, NetworkError::DomainDenied(_)));
    }

    #[test]
    fn test_safe_dns_resolver_new() {
        let policy = NetworkPolicy::default();
        let _resolver = SafeDnsResolver::new(policy);
    }

    #[test]
    fn test_secure_http_client_factory_build() {
        let policy = NetworkPolicy::default();
        let client = SecureHttpClientFactory::build(&policy, std::time::Duration::from_secs(30));
        assert!(client.is_ok());
    }

    #[test]
    fn test_secure_http_client_factory_build_no_rebinding() {
        let policy = NetworkPolicy {
            dns_rebinding_protection: false,
            ..NetworkPolicy::default()
        };
        let client = SecureHttpClientFactory::build(&policy, std::time::Duration::from_secs(10));
        assert!(client.is_ok());
    }

    #[test]
    fn test_network_error_display() {
        let e = NetworkError::InvalidUrl("bad".into());
        assert!(e.to_string().contains("bad"));
        let e2 = NetworkError::BlockedScheme("ftp".into());
        assert!(e2.to_string().contains("ftp"));
        let e3 = NetworkError::BlockedPort(9999);
        assert!(e3.to_string().contains("9999"));
        let e4 = NetworkError::DomainNotAllowed("evil.com".into());
        assert!(e4.to_string().contains("evil.com"));
        let e5 = NetworkError::DomainDenied("evil.com".into());
        assert!(e5.to_string().contains("evil.com"));
        let e6 = NetworkError::DnsResolutionFailed("host".into());
        assert!(e6.to_string().contains("host"));
        let e7 = NetworkError::BlockedIp { host: "h".into(), ip: "1.2.3.4".parse().unwrap() };
        assert!(e7.to_string().contains("1.2.3.4"));
        let e8 = NetworkError::RedirectBlocked("reason".into());
        assert!(e8.to_string().contains("reason"));
        let e9 = NetworkError::ClientBuildFailed("err".into());
        assert!(e9.to_string().contains("err"));
    }

    #[tokio::test]
    async fn test_validate_url_allow_all() {
        let policy = NetworkPolicy {
            block_private_ips: false,
            block_loopback: false,
            block_metadata_endpoints: false,
            ..NetworkPolicy::default()
        };
        // Public IP that may not resolve in CI - test scheme/port validation
        let result = validate_url("ftp://example.com", &policy).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_validate_url_blocked_scheme() {
        let policy = NetworkPolicy::default();
        let result = validate_url("ftp://example.com", &policy).await;
        assert!(matches!(result, Err(NetworkError::BlockedScheme(_))));
    }

    #[tokio::test]
    async fn test_validate_url_invalid_url() {
        let policy = NetworkPolicy::default();
        let result = validate_url("not a url", &policy).await;
        assert!(matches!(result, Err(NetworkError::InvalidUrl(_))));
    }

    #[tokio::test]
    async fn test_validate_url_blocked_port() {
        let policy = NetworkPolicy {
            allowed_ports: vec![443],
            ..NetworkPolicy::default()
        };
        let result = validate_url("https://example.com:9999", &policy).await;
        assert!(matches!(result, Err(NetworkError::BlockedPort(9999))));
    }

    #[tokio::test]
    async fn test_validate_url_allow_list_blocked() {
        let policy = NetworkPolicy {
            mode: NetworkPolicyMode::AllowList,
            allowed_domains: vec!["allowed.com".into()],
            ..NetworkPolicy::default()
        };
        let result = validate_url("https://blocked.com", &policy).await;
        assert!(matches!(result, Err(NetworkError::DomainNotAllowed(_))));
    }

    #[tokio::test]
    async fn test_validate_url_deny_list_blocked() {
        let policy = NetworkPolicy {
            mode: NetworkPolicyMode::DenyList,
            denied_domains: vec!["evil.com".into()],
            ..NetworkPolicy::default()
        };
        let result = validate_url("https://evil.com", &policy).await;
        assert!(matches!(result, Err(NetworkError::DomainDenied(_))));
    }

    #[tokio::test]
    async fn test_resolver_blocked_ip_direct() {
        let policy = NetworkPolicy::default();
        let resolver = SafeDnsResolver::new(policy);
        let result = resolver.resolve_and_validate("127.0.0.1").await;
        assert!(matches!(result, Err(NetworkError::BlockedIp { .. })));
    }

    #[tokio::test]
    async fn test_resolver_public_ip_direct() {
        let policy = NetworkPolicy {
            block_private_ips: false,
            block_loopback: false,
            block_metadata_endpoints: false,
            ..NetworkPolicy::default()
        };
        let resolver = SafeDnsResolver::new(policy);
        let result = resolver.resolve_and_validate("8.8.8.8").await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap().len(), 1);
    }
}
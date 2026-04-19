//! Transport layer abstraction for the MCP server.
//!
//! The MCP server can listen on either stdio (one client at a time) or
//! Streamable HTTP (many clients simultaneously). Transport selection is
//! driven by CLI flags / `kb-mcp.toml`, resolved into a [`Transport`] enum
//! and then dispatched to the corresponding runner in [`stdio`] / [`http`].

use std::net::SocketAddr;

use anyhow::Result;
use serde::Deserialize;

pub mod http;
pub mod stdio;

// ---------------------------------------------------------------------------
// CLI / config enums
// ---------------------------------------------------------------------------

/// CLI-level transport selector.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum TransportKind {
    Stdio,
    Http,
}

/// `[transport].kind` の config 表現。`clap::ValueEnum` と独立の型に
/// しておくと config 側で deny_unknown_fields が素直に効く。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TransportKindConfig {
    Stdio,
    Http,
}

/// `[transport.http]` config.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct HttpTransportConfig {
    /// `127.0.0.1:3100` 等の SocketAddr 文字列 (bind address)。
    #[serde(default)]
    pub bind: Option<String>,
}

/// `[transport]` config section.
#[derive(Debug, Clone, Default, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct TransportConfig {
    #[serde(default)]
    pub kind: Option<TransportKindConfig>,
    #[serde(default)]
    pub http: Option<HttpTransportConfig>,
}

// ---------------------------------------------------------------------------
// Runtime transport choice
// ---------------------------------------------------------------------------

/// Resolved transport to use at runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Transport {
    Stdio,
    Http { addr: SocketAddr },
}

const DEFAULT_HTTP_PORT: u16 = 3100;

impl Transport {
    /// Resolve `Transport` from CLI + config + defaults, in that priority order.
    ///
    /// - CLI `--transport` wins over config
    /// - `[transport.http]` 単独指定 (kind 省略) は HTTP 扱い (糖衣)
    /// - HTTP bind 解決: `--bind` (完全形) > `(127.0.0.1, --port)` > config bind > `127.0.0.1:3100`
    pub fn resolve(
        cli_transport: Option<TransportKind>,
        cli_bind: Option<SocketAddr>,
        cli_port: Option<u16>,
        cfg: Option<&TransportConfig>,
    ) -> Result<Self> {
        let kind = cli_transport
            .map(|t| match t {
                TransportKind::Stdio => TransportKindConfig::Stdio,
                TransportKind::Http => TransportKindConfig::Http,
            })
            .or_else(|| cfg.and_then(|c| c.kind))
            .or_else(|| {
                // [transport.http] があれば kind 未指定でも Http と解釈
                if cfg.is_some_and(|c| c.http.is_some()) {
                    Some(TransportKindConfig::Http)
                } else {
                    None
                }
            })
            .unwrap_or(TransportKindConfig::Stdio);

        match kind {
            TransportKindConfig::Stdio => Ok(Transport::Stdio),
            TransportKindConfig::Http => {
                let addr = resolve_http_addr(cli_bind, cli_port, cfg)?;
                Ok(Transport::Http { addr })
            }
        }
    }
}

fn resolve_http_addr(
    cli_bind: Option<SocketAddr>,
    cli_port: Option<u16>,
    cfg: Option<&TransportConfig>,
) -> Result<SocketAddr> {
    if let Some(bind) = cli_bind {
        return Ok(bind);
    }
    if let Some(port) = cli_port {
        return Ok(SocketAddr::from(([127, 0, 0, 1], port)));
    }
    if let Some(bind_str) = cfg.and_then(|c| c.http.as_ref()).and_then(|h| h.bind.as_deref()) {
        return bind_str
            .parse()
            .map_err(|e| anyhow::anyhow!("[transport.http].bind is not a valid SocketAddr: {bind_str}: {e}"));
    }
    Ok(SocketAddr::from(([127, 0, 0, 1], DEFAULT_HTTP_PORT)))
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_default_is_stdio() {
        let t = Transport::resolve(None, None, None, None).unwrap();
        assert_eq!(t, Transport::Stdio);
    }

    #[test]
    fn test_resolve_cli_http_default_bind() {
        let t = Transport::resolve(Some(TransportKind::Http), None, None, None).unwrap();
        assert_eq!(
            t,
            Transport::Http {
                addr: "127.0.0.1:3100".parse().unwrap(),
            }
        );
    }

    #[test]
    fn test_resolve_cli_port_only() {
        let t = Transport::resolve(Some(TransportKind::Http), None, Some(4000), None).unwrap();
        assert_eq!(
            t,
            Transport::Http {
                addr: "127.0.0.1:4000".parse().unwrap(),
            }
        );
    }

    #[test]
    fn test_resolve_cli_bind_full_wins() {
        let t = Transport::resolve(
            Some(TransportKind::Http),
            Some("0.0.0.0:9000".parse().unwrap()),
            Some(4000), // should be overridden by --bind
            None,
        )
        .unwrap();
        assert_eq!(
            t,
            Transport::Http {
                addr: "0.0.0.0:9000".parse().unwrap(),
            }
        );
    }

    #[test]
    fn test_resolve_cli_overrides_config() {
        let cfg = TransportConfig {
            kind: Some(TransportKindConfig::Http),
            http: None,
        };
        // CLI stdio wins over config http
        let t = Transport::resolve(Some(TransportKind::Stdio), None, None, Some(&cfg)).unwrap();
        assert_eq!(t, Transport::Stdio);
    }

    #[test]
    fn test_resolve_http_section_implies_http_kind() {
        // [transport.http] だけ書かれていれば kind 省略でも Http 扱い
        let cfg = TransportConfig {
            kind: None,
            http: Some(HttpTransportConfig {
                bind: Some("127.0.0.1:5555".into()),
            }),
        };
        let t = Transport::resolve(None, None, None, Some(&cfg)).unwrap();
        assert_eq!(
            t,
            Transport::Http {
                addr: "127.0.0.1:5555".parse().unwrap(),
            }
        );
    }

    #[test]
    fn test_resolve_config_bind_malformed_is_error() {
        let cfg = TransportConfig {
            kind: Some(TransportKindConfig::Http),
            http: Some(HttpTransportConfig {
                bind: Some("not-an-address".into()),
            }),
        };
        let err = Transport::resolve(None, None, None, Some(&cfg)).expect_err("must reject");
        assert!(err.to_string().contains("SocketAddr"));
    }

    #[test]
    fn test_resolve_config_stdio() {
        let cfg = TransportConfig {
            kind: Some(TransportKindConfig::Stdio),
            http: None,
        };
        let t = Transport::resolve(None, None, None, Some(&cfg)).unwrap();
        assert_eq!(t, Transport::Stdio);
    }
}

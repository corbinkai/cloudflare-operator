use serde::{Deserialize, Serialize};

/// Generic Cloudflare API response envelope for single results.
#[derive(Deserialize)]
pub struct CfResponse<T> {
    pub success: bool,
    pub errors: Vec<CfError>,
    pub result: Option<T>,
}

/// Generic Cloudflare API response envelope for list results.
#[derive(Deserialize)]
pub struct CfListResponse<T> {
    pub success: bool,
    pub errors: Vec<CfError>,
    pub result: Vec<T>,
    pub result_info: Option<ResultInfo>,
}

/// A single error from the Cloudflare API.
#[derive(Deserialize)]
pub struct CfError {
    pub code: u64,
    pub message: String,
}

/// Pagination info returned with list responses.
#[derive(Deserialize)]
pub struct ResultInfo {
    pub page: u32,
    pub per_page: u32,
    pub count: u32,
    pub total_count: u32,
}

/// A Cloudflare Tunnel as returned by the API.
#[derive(Deserialize)]
pub struct CfTunnel {
    pub id: String,
    pub name: String,
}

/// Request body for creating a tunnel.
#[derive(Serialize)]
pub struct CreateTunnelRequest {
    pub name: String,
    pub tunnel_secret: String,
    pub config_src: String,
}

/// A Cloudflare DNS record as returned by the API.
#[derive(Deserialize, Clone)]
pub struct DnsRecord {
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub record_type: String,
    pub content: String,
}

/// Request body for creating a DNS record.
#[derive(Serialize)]
pub struct CreateDnsRecordRequest {
    #[serde(rename = "type")]
    pub record_type: String,
    pub name: String,
    pub content: String,
    pub ttl: u32,
    pub proxied: bool,
    pub comment: String,
}

/// Request body for updating a DNS record.
#[derive(Serialize)]
pub struct UpdateDnsRecordRequest {
    #[serde(rename = "type")]
    pub record_type: String,
    pub name: String,
    pub content: String,
    pub ttl: u32,
    pub proxied: bool,
    pub comment: String,
}

/// Tunnel configuration request for the edge push API.
#[derive(Serialize)]
pub struct TunnelConfigurationRequest {
    pub config: TunnelConfig,
}

/// Tunnel configuration containing ingress rules.
#[derive(Serialize)]
pub struct TunnelConfig {
    pub ingress: Vec<TunnelIngressRule>,
}

/// A single tunnel ingress rule.
#[derive(Clone, Serialize)]
pub struct TunnelIngressRule {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hostname: Option<String>,
    pub service: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

/// A Cloudflare account as returned by the API.
#[derive(Deserialize)]
pub struct Account {
    pub id: String,
    pub name: String,
}

/// A Cloudflare zone as returned by the API.
#[derive(Deserialize)]
pub struct Zone {
    pub id: String,
    pub name: String,
}

/// JSON content stored in managed TXT records to track DNS ownership.
#[derive(Serialize, Deserialize)]
pub struct DnsManagedRecordTxt {
    #[serde(rename = "DnsId")]
    pub dns_id: String,
    #[serde(rename = "TunnelName")]
    pub tunnel_name: String,
    #[serde(rename = "TunnelId")]
    pub tunnel_id: String,
}

/// Tunnel credentials file written to the cloudflared secret.
#[derive(Serialize, Deserialize)]
pub struct TunnelCredentials {
    #[serde(rename = "AccountTag")]
    pub account_tag: String,
    #[serde(rename = "TunnelID")]
    pub tunnel_id: String,
    #[serde(rename = "TunnelName")]
    pub tunnel_name: String,
    #[serde(rename = "TunnelSecret")]
    pub tunnel_secret: String,
}

use kube::CustomResource;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// AccessTunnelServiceConfig defines the Service created for the AccessTunnel.
#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema)]
pub struct AccessTunnelServiceConfig {
    /// Name of the new service to create.
    /// Defaults to the name of the AccessTunnel object.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,

    /// Service port to expose with. Defaults to 8000.
    #[serde(default = "default_port")]
    #[schemars(range(min = 1, max = 65535))]
    pub port: i32,
}

fn default_port() -> i32 {
    8000
}

/// AccessTunnelTarget defines the desired state of the AccessTunnel.
#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema)]
pub struct AccessTunnelTarget {
    /// cloudflared image to use
    #[serde(default = "default_image")]
    pub image: String,

    /// Fqdn specifies the DNS name to access
    pub fqdn: String,

    /// Protocol to forward: tcp, rdp, smb, or ssh
    #[serde(default = "default_protocol")]
    pub protocol: String,

    /// Service Config
    #[serde(default)]
    pub svc: AccessTunnelServiceConfig,
}

fn default_image() -> String {
    "cloudflare/cloudflared:2025.4.0".to_string()
}

fn default_protocol() -> String {
    "tcp".to_string()
}

/// AccessTunnelServiceToken defines the access auth if needed.
#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema)]
#[allow(non_snake_case)]
pub struct AccessTunnelServiceToken {
    /// Access Service Token Secret reference
    #[serde(rename = "secretRef")]
    pub secret_ref: String,

    /// Key in the secret to use for Access Service Token ID
    #[serde(default = "default_token_id_key")]
    pub CLOUDFLARE_ACCESS_SERVICE_TOKEN_ID: String,

    /// Key in the secret to use for Access Service Token Token
    #[serde(default = "default_token_token_key")]
    pub CLOUDFLARE_ACCESS_SERVICE_TOKEN_TOKEN: String,
}

fn default_token_id_key() -> String {
    "CLOUDFLARE_ACCESS_SERVICE_TOKEN_ID".to_string()
}

fn default_token_token_key() -> String {
    "CLOUDFLARE_ACCESS_SERVICE_TOKEN_TOKEN".to_string()
}

/// AccessTunnelSpec defines the desired state of AccessTunnel.
#[derive(CustomResource, Clone, Debug, Default, Deserialize, Serialize, JsonSchema)]
#[kube(
    group = "networking.cfargotunnel.com",
    version = "v1alpha1",
    kind = "AccessTunnel",
    namespaced,
    status = "AccessTunnelStatus",
    printcolumn = r#"{"name":"Target","type":"string","jsonPath":".target.fqdn"}"#
)]
pub struct AccessTunnelSpec {
    /// Target defines the service to expose
    pub target: AccessTunnelTarget,

    /// ServiceToken defines the access auth if needed
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "serviceToken")]
    pub service_token: Option<AccessTunnelServiceToken>,
}

/// AccessTunnelStatus defines the observed state of AccessTunnel.
#[derive(Clone, Debug, Default, Deserialize, Serialize, JsonSchema)]
pub struct AccessTunnelStatus {}

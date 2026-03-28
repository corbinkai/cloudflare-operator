use kube::runtime::events::{Recorder, Reporter};

/// Shared context for all controllers.
pub struct Context {
    /// Kubernetes client
    pub client: kube::Client,

    /// Reporter used for Kubernetes events
    pub reporter: Reporter,

    /// Optional override for Cloudflare API base URL (for testing)
    pub cloudflare_api_base_url: Option<String>,

    /// Namespace where ClusterTunnel resources are deployed
    pub cluster_resource_namespace: String,

    /// Whether to overwrite DNS records not created by this operator
    pub overwrite_unmanaged: bool,

    /// Image to use for the cloudflared container
    pub cloudflared_image: String,

    /// Whether the Gateway API controllers are enabled at runtime
    #[cfg(feature = "gateway-api")]
    pub enable_gateway_api: bool,
}

impl Context {
    pub fn recorder(&self) -> Recorder {
        Recorder::new(self.client.clone(), self.reporter.clone())
    }
}

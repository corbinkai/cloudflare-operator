/// Shared context for all controllers.
pub struct Context {
    /// Kubernetes client
    pub client: kube::Client,

    /// Optional override for Cloudflare API base URL (for testing)
    pub cloudflare_api_base_url: Option<String>,

    /// Namespace where ClusterTunnel resources are deployed
    pub cluster_resource_namespace: String,

    /// Whether to overwrite DNS records not created by this operator
    pub overwrite_unmanaged: bool,

    /// Image to use for the cloudflared container
    pub cloudflared_image: String,
}

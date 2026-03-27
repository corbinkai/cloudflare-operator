use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::core::v1::{ConfigMap, Secret, Service};
use kube::api::{Api, ListParams, Patch, PatchParams};
use kube::runtime::controller::Action;
use kube::ResourceExt;
use md5::{Digest, Md5};
use tracing::{error, info, warn};

use crate::cloudflare::client::CfClient;
use crate::cloudflare::types::TunnelIngressRule;
use crate::config::cloudflared::{Configuration, OriginRequestConfig, UnvalidatedIngressRule};
use crate::crds::cluster_tunnel::ClusterTunnel;
use crate::crds::tunnel::Tunnel;
use crate::crds::tunnel_binding::{TunnelBinding, TunnelBindingStatus};
use crate::crds::types::{CloudflareDetails, ServiceInfo};
use crate::error::Error;

use super::context::Context;

const TUNNEL_FINALIZER: &str = "cfargotunnel.com/finalizer";
const CONFIGMAP_KEY: &str = "config.yaml";
const TUNNEL_CONFIG_CHECKSUM: &str = "cfargotunnel.com/checksum";
const TUNNEL_NAME_LABEL: &str = "cfargotunnel.com/name";
const TUNNEL_KIND_LABEL: &str = "cfargotunnel.com/kind";

const VALID_PROTOCOLS: &[&str] = &["http", "https", "rdp", "smb", "ssh", "tcp", "udp"];

/// Resolved tunnel info needed for reconciliation.
struct TunnelInfo {
    fallback_target: String,
    cloudflare: CloudflareDetails,
    tunnel_id: String,
    tunnel_name: String,
    account_id: String,
    zone_id: String,
    tunnel_ns: String,
}

pub async fn reconcile_binding(
    obj: Arc<TunnelBinding>,
    ctx: Arc<Context>,
) -> Result<Action, Error> {
    let k8s = &ctx.client;
    let binding_ns = obj.metadata.namespace.clone().unwrap_or_default();
    let binding_name = obj.name_any();

    info!(name = %binding_name, ns = %binding_ns, "reconciling tunnel binding");

    // 1. Look up the referenced Tunnel or ClusterTunnel
    let tunnel_info = get_tunnel_info(k8s, &obj, &binding_ns, &ctx.cluster_resource_namespace).await?;

    // 2. Read CF credentials from the tunnel's Secret (or credential_secret_ref override)
    let (secret_name, secret_ns) = match &obj.tunnel_ref.credential_secret_ref {
        Some(secret_ref) => (secret_ref.name.clone(), secret_ref.namespace.clone()),
        None => (tunnel_info.cloudflare.secret.clone(), tunnel_info.tunnel_ns.clone()),
    };

    let secrets_api: Api<Secret> = Api::namespaced(k8s.clone(), &secret_ns);
    let cf_secret = secrets_api.get(&secret_name).await.map_err(|e| {
        error!(secret = %secret_name, ns = %secret_ns, error = %e, "failed to read cloudflare secret");
        e
    })?;

    // 3. Build CfClient
    let mut cf_client = build_cf_client(
        &tunnel_info.cloudflare,
        &cf_secret,
        ctx.cloudflare_api_base_url.as_deref(),
    )?;
    cf_client.domain = tunnel_info.cloudflare.domain.clone();
    cf_client.account_id = tunnel_info.account_id.clone();
    cf_client.tunnel_id = tunnel_info.tunnel_id.clone();
    cf_client.tunnel_name = tunnel_info.tunnel_name.clone();
    cf_client.zone_id = tunnel_info.zone_id.clone();

    // Validate zone so DNS operations work
    cf_client
        .validate_zone(&tunnel_info.cloudflare.domain)
        .await?;

    // 4. Get the tunnel's ConfigMap
    let cm_api: Api<ConfigMap> = Api::namespaced(k8s.clone(), &tunnel_info.tunnel_ns);
    let configmap = cm_api.get(&obj.tunnel_ref.name).await.map_err(|e| {
        error!(
            configmap = %obj.tunnel_ref.name,
            ns = %tunnel_info.tunnel_ns,
            error = %e,
            "failed to get tunnel configmap"
        );
        e
    })?;

    // 5. If deletion timestamp set, run deletion logic
    if obj.metadata.deletion_timestamp.is_some() {
        return handle_deletion(k8s, &obj, &binding_ns, &cf_client).await;
    }

    // 6. Compute status: resolve each subject to hostname + target
    let old_services = obj
        .status
        .as_ref()
        .map(|s| s.services.clone())
        .unwrap_or_default();

    let services = resolve_subjects(k8s, &obj, &binding_ns, &cf_client).await?;

    // 7. Update status on the TunnelBinding
    update_binding_status(k8s, &binding_name, &binding_ns, &services).await?;

    // 8. Configure cloudflared daemon (rebuild ConfigMap ingress)
    configure_cloudflare_daemon(
        k8s,
        &obj.tunnel_ref.name,
        &obj.tunnel_ref.kind,
        &tunnel_info.tunnel_ns,
        &tunnel_info.fallback_target,
        &cf_client,
        &configmap,
    )
    .await?;

    // 9. Creation logic: set labels, finalizer, DNS records
    handle_creation(
        k8s,
        &obj,
        &binding_name,
        &binding_ns,
        &services,
        &old_services,
        &cf_client,
        ctx.overwrite_unmanaged,
    )
    .await?;

    Ok(Action::requeue(Duration::from_secs(300)))
}

pub fn binding_error_policy(
    _obj: Arc<TunnelBinding>,
    error: &Error,
    _ctx: Arc<Context>,
) -> Action {
    error!(error = %error, "tunnel binding reconciliation error, will retry");
    Action::requeue(Duration::from_secs(15))
}

// ── Get tunnel info ─────────────────────────────────────────────────────

async fn get_tunnel_info(
    k8s: &kube::Client,
    binding: &TunnelBinding,
    binding_ns: &str,
    cluster_resource_namespace: &str,
) -> Result<TunnelInfo, Error> {
    match binding.tunnel_ref.kind.to_lowercase().as_str() {
        "tunnel" => {
            let api: Api<Tunnel> = Api::namespaced(k8s.clone(), binding_ns);
            let tunnel = api.get(&binding.tunnel_ref.name).await?;
            let status = tunnel.status.clone().unwrap_or_default();
            Ok(TunnelInfo {
                fallback_target: tunnel.spec.fallback_target.clone(),
                cloudflare: tunnel.spec.cloudflare.clone(),
                tunnel_id: status.tunnel_id,
                tunnel_name: status.tunnel_name,
                account_id: status.account_id,
                zone_id: status.zone_id,
                tunnel_ns: binding_ns.to_string(),
            })
        }
        "clustertunnel" => {
            let api: Api<ClusterTunnel> = Api::all(k8s.clone());
            let ct = api.get(&binding.tunnel_ref.name).await?;
            let status = ct.status.clone().unwrap_or_default();
            Ok(TunnelInfo {
                fallback_target: ct.spec.fallback_target.clone(),
                cloudflare: ct.spec.cloudflare.clone(),
                tunnel_id: status.tunnel_id,
                tunnel_name: status.tunnel_name,
                account_id: status.account_id,
                zone_id: status.zone_id,
                tunnel_ns: cluster_resource_namespace.to_string(),
            })
        }
        other => Err(Error::Config(format!(
            "unsupported tunnelRef kind: {other}"
        ))),
    }
}

// ── Build CfClient from K8s Secret ──────────────────────────────────────

fn build_cf_client(
    cf: &CloudflareDetails,
    secret: &Secret,
    base_url: Option<&str>,
) -> Result<CfClient, Error> {
    let data = secret
        .data
        .as_ref()
        .ok_or_else(|| Error::MissingField("cloudflare secret has no data".into()))?;

    let api_token = data
        .get(&cf.cloudflare_api_token)
        .map(|b| String::from_utf8_lossy(&b.0).to_string());
    let api_key = data
        .get(&cf.cloudflare_api_key)
        .map(|b| String::from_utf8_lossy(&b.0).to_string());

    if api_token.is_none() && api_key.is_none() {
        return Err(Error::MissingField(format!(
            "neither {} nor {} found in secret {}",
            cf.cloudflare_api_token, cf.cloudflare_api_key, cf.secret
        )));
    }

    if let Some(token) = api_token {
        Ok(CfClient::new(&token, base_url))
    } else {
        let key = api_key.unwrap();
        Ok(CfClient::new_with_key(&key, &cf.email, base_url))
    }
}

// ── Resolve subjects to ServiceInfo ─────────────────────────────────────

async fn resolve_subjects(
    k8s: &kube::Client,
    binding: &TunnelBinding,
    binding_ns: &str,
    cf_client: &CfClient,
) -> Result<Vec<ServiceInfo>, Error> {
    let svc_api: Api<Service> = Api::namespaced(k8s.clone(), binding_ns);
    let mut services = Vec::with_capacity(binding.subjects.len());

    for subject in &binding.subjects {
        let hostname = if !subject.spec.fqdn.is_empty() {
            subject.spec.fqdn.clone()
        } else {
            format!("{}.{}", subject.name, cf_client.domain)
        };

        let target = if !subject.spec.target.is_empty() {
            subject.spec.target.clone()
        } else {
            match svc_api.get(&subject.name).await {
                Ok(svc) => resolve_service_target(&svc, &subject.spec.protocol, binding_ns),
                Err(e) => {
                    warn!(
                        service = %subject.name,
                        error = %e,
                        "failed to get service, using fallback target"
                    );
                    "http_status:404".to_string()
                }
            }
        };

        services.push(ServiceInfo { hostname, target });
    }

    Ok(services)
}

fn resolve_service_target(svc: &Service, protocol_override: &str, ns: &str) -> String {
    let svc_name = svc.metadata.name.as_deref().unwrap_or("unknown");

    let ports = svc
        .spec
        .as_ref()
        .and_then(|s| s.ports.as_ref())
        .cloned()
        .unwrap_or_default();

    if ports.is_empty() {
        warn!(service = %svc_name, "service has no ports, using fallback");
        return "http_status:404".to_string();
    }

    if ports.len() > 1 {
        info!(service = %svc_name, "multiple ports found, using the first");
    }

    let port = &ports[0];
    let port_number = port.port;
    let port_protocol = port.protocol.as_deref().unwrap_or("TCP");

    let svc_proto = select_protocol(protocol_override, port_number, port_protocol);

    format!("{svc_proto}://{svc_name}.{ns}.svc:{port_number}")
}

fn select_protocol(tunnel_proto: &str, port: i32, port_protocol: &str) -> String {
    if !tunnel_proto.is_empty() && VALID_PROTOCOLS.contains(&tunnel_proto) {
        return tunnel_proto.to_string();
    }

    if !tunnel_proto.is_empty() {
        warn!(protocol = %tunnel_proto, "invalid protocol provided, using default logic");
    }

    if port_protocol == "TCP" {
        match port {
            22 => "ssh".to_string(),
            139 | 445 => "smb".to_string(),
            443 => "https".to_string(),
            3389 => "rdp".to_string(),
            _ => "http".to_string(),
        }
    } else if port_protocol == "UDP" {
        "udp".to_string()
    } else {
        "http".to_string()
    }
}

// ── Update binding status ───────────────────────────────────────────────

async fn update_binding_status(
    k8s: &kube::Client,
    name: &str,
    ns: &str,
    services: &[ServiceInfo],
) -> Result<(), Error> {
    let hostnames = services
        .iter()
        .map(|s| s.hostname.as_str())
        .collect::<Vec<_>>()
        .join(",");

    let status = TunnelBindingStatus {
        hostnames,
        services: services.to_vec(),
    };

    let patch = serde_json::json!({ "status": status });
    let api: Api<TunnelBinding> = Api::namespaced(k8s.clone(), ns);
    api.patch_status(
        name,
        &PatchParams::apply("cloudflare-operator"),
        &Patch::Merge(&patch),
    )
    .await?;

    info!(name = %name, "binding status updated");
    Ok(())
}

// ── Deletion logic ──────────────────────────────────────────────────────

async fn handle_deletion(
    k8s: &kube::Client,
    binding: &TunnelBinding,
    binding_ns: &str,
    cf_client: &CfClient,
) -> Result<Action, Error> {
    let name = binding.name_any();
    let has_finalizer = binding
        .metadata
        .finalizers
        .as_ref()
        .map_or(false, |f| f.contains(&TUNNEL_FINALIZER.to_string()));

    if !has_finalizer {
        return Ok(Action::requeue(Duration::from_secs(1)));
    }

    info!(name = %name, "running deletion logic for tunnel binding");

    // Delete DNS for each hostname in status
    let mut had_errors = false;
    if let Some(status) = &binding.status {
        for svc in &status.services {
            if svc.hostname.is_empty() {
                continue;
            }
            if let Err(e) = delete_dns_for_hostname(cf_client, &svc.hostname).await {
                error!(hostname = %svc.hostname, error = %e, "failed to delete DNS during finalization");
                had_errors = true;
            }
        }
    }

    if had_errors {
        return Err(Error::Cloudflare(
            "errors occurred during DNS cleanup, will retry".into(),
        ));
    }

    // Remove finalizer
    let new_finalizers: Vec<String> = binding
        .metadata
        .finalizers
        .as_ref()
        .map(|f| {
            f.iter()
                .filter(|fin| fin.as_str() != TUNNEL_FINALIZER)
                .cloned()
                .collect()
        })
        .unwrap_or_default();

    let patch = serde_json::json!({
        "metadata": {
            "finalizers": new_finalizers
        }
    });
    let api: Api<TunnelBinding> = Api::namespaced(k8s.clone(), binding_ns);
    api.patch(
        &name,
        &PatchParams::apply("cloudflare-operator"),
        &Patch::Merge(&patch),
    )
    .await?;

    info!(name = %name, "finalizer removed from tunnel binding");
    Ok(Action::requeue(Duration::from_secs(1)))
}

async fn delete_dns_for_hostname(cf_client: &CfClient, hostname: &str) -> Result<(), Error> {
    let (txt_id, txt_data, can_use) = match cf_client.get_managed_dns_txt(hostname).await {
        Ok(result) => result,
        Err(e) => {
            warn!(hostname = %hostname, error = %e, "failed to read managed TXT, skipping DNS cleanup");
            return Ok(());
        }
    };

    if !can_use {
        warn!(hostname = %hostname, "TXT record belongs to different tunnel, skipping cleanup");
        return Ok(());
    }

    // Verify CNAME ID matches before deleting
    if !txt_data.dns_id.is_empty() {
        match cf_client.get_dns_cname_id(hostname).await {
            Ok(cname_id) => {
                if cname_id != txt_data.dns_id {
                    error!(
                        hostname = %hostname,
                        cname_id = %cname_id,
                        txt_dns_id = %txt_data.dns_id,
                        "DNS ID from TXT and real DNS record do not match"
                    );
                    return Err(Error::Cloudflare(format!(
                        "DNS/TXT ID mismatch for {hostname}"
                    )));
                }
                cf_client
                    .delete_dns_id(hostname, &txt_data.dns_id, true)
                    .await?;
                info!(hostname = %hostname, "deleted DNS CNAME record");
            }
            Err(e) => {
                warn!(hostname = %hostname, error = %e, "error fetching DNS CNAME record");
            }
        }
    }

    if !txt_id.is_empty() {
        cf_client.delete_dns_id(hostname, &txt_id, true).await?;
        info!(hostname = %hostname, "deleted DNS TXT record");
    }

    Ok(())
}

// ── Creation logic ──────────────────────────────────────────────────────

async fn handle_creation(
    k8s: &kube::Client,
    binding: &TunnelBinding,
    name: &str,
    ns: &str,
    new_services: &[ServiceInfo],
    old_services: &[ServiceInfo],
    cf_client: &CfClient,
    overwrite_unmanaged: bool,
) -> Result<(), Error> {
    let api: Api<TunnelBinding> = Api::namespaced(k8s.clone(), ns);

    // Set labels
    let mut labels: BTreeMap<String, String> = binding
        .metadata
        .labels
        .clone()
        .unwrap_or_default();
    labels.insert(
        TUNNEL_NAME_LABEL.to_string(),
        binding.tunnel_ref.name.clone(),
    );
    labels.insert(
        TUNNEL_KIND_LABEL.to_string(),
        binding.tunnel_ref.kind.clone(),
    );

    let label_patch = serde_json::json!({
        "metadata": { "labels": labels }
    });
    api.patch(
        name,
        &PatchParams::apply("cloudflare-operator"),
        &Patch::Merge(&label_patch),
    )
    .await?;

    // If DNS updates disabled, stop here
    if binding.tunnel_ref.disable_dns_updates {
        return Ok(());
    }

    // Add finalizer if not present
    let has_finalizer = binding
        .metadata
        .finalizers
        .as_ref()
        .map_or(false, |f| f.contains(&TUNNEL_FINALIZER.to_string()));

    if !has_finalizer {
        let mut finalizers = binding.metadata.finalizers.clone().unwrap_or_default();
        finalizers.push(TUNNEL_FINALIZER.to_string());
        let finalizer_patch = serde_json::json!({
            "metadata": { "finalizers": finalizers }
        });
        api.patch(
            name,
            &PatchParams::apply("cloudflare-operator"),
            &Patch::Merge(&finalizer_patch),
        )
        .await?;
        info!(name = %name, "finalizer added to tunnel binding");
    }

    // Create/update DNS entries for each hostname
    let mut had_errors = false;
    for svc in new_services {
        if svc.hostname.is_empty() {
            continue;
        }
        if let Err(e) = create_dns_for_hostname(cf_client, &svc.hostname, overwrite_unmanaged).await
        {
            error!(hostname = %svc.hostname, error = %e, "failed to create/update DNS");
            had_errors = true;
        }
    }

    // Bug fix #12: detect FQDN changes and clean up old hostnames
    let new_hostnames: HashSet<&str> = new_services.iter().map(|s| s.hostname.as_str()).collect();
    for old_svc in old_services {
        if old_svc.hostname.is_empty() {
            continue;
        }
        if !new_hostnames.contains(old_svc.hostname.as_str()) {
            info!(hostname = %old_svc.hostname, "FQDN changed, cleaning up old DNS");
            if let Err(e) = delete_dns_for_hostname(cf_client, &old_svc.hostname).await {
                warn!(hostname = %old_svc.hostname, error = %e, "failed to clean up old DNS");
                had_errors = true;
            }
        }
    }

    if had_errors {
        return Err(Error::Cloudflare(
            "some DNS entries failed to create/update".into(),
        ));
    }

    Ok(())
}

async fn create_dns_for_hostname(
    cf_client: &CfClient,
    hostname: &str,
    overwrite_unmanaged: bool,
) -> Result<(), Error> {
    // Check managed TXT record
    let (txt_id, mut txt_data, can_use) = cf_client.get_managed_dns_txt(hostname).await?;
    if !can_use {
        return Err(Error::Cloudflare(format!(
            "FQDN {hostname} already managed by tunnel {} ({})",
            txt_data.tunnel_name, txt_data.tunnel_id
        )));
    }

    // Check if a CNAME record already exists
    match cf_client.get_dns_cname_id(hostname).await {
        Ok(existing_id) if !existing_id.is_empty() => {
            // CNAME exists without a managed TXT record and we should not overwrite
            if !overwrite_unmanaged && txt_id.is_empty() {
                return Err(Error::Cloudflare(format!(
                    "unmanaged FQDN {hostname} present and overwrite_unmanaged is false"
                )));
            }
            // Use existing ID for update
            txt_data.dns_id = existing_id;
        }
        _ => {}
    }

    // Create/update CNAME
    let new_dns_id = cf_client
        .insert_or_update_cname(hostname, &txt_data.dns_id)
        .await?;

    // Create/update TXT
    if let Err(e) = cf_client
        .insert_or_update_txt(hostname, &txt_id, &new_dns_id)
        .await
    {
        error!(hostname = %hostname, error = %e, "failed to insert/update TXT entry");
        // Roll back CNAME if it was newly created (dns_id was empty before)
        if let Err(del_err) = cf_client
            .delete_dns_id(hostname, &new_dns_id, !txt_data.dns_id.is_empty())
            .await
        {
            error!(hostname = %hostname, error = %del_err, "failed to roll back CNAME after TXT failure");
        }
        return Err(e);
    }

    info!(hostname = %hostname, "DNS CNAME+TXT records created/updated");
    Ok(())
}

// ── Configure cloudflared daemon (rebuild ConfigMap) ─────────────────────

async fn configure_cloudflare_daemon(
    k8s: &kube::Client,
    tunnel_name: &str,
    tunnel_kind: &str,
    tunnel_ns: &str,
    fallback_target: &str,
    cf_client: &CfClient,
    configmap: &ConfigMap,
) -> Result<(), Error> {
    // Parse existing config
    let config_str = configmap
        .data
        .as_ref()
        .and_then(|d| d.get(CONFIGMAP_KEY))
        .ok_or_else(|| {
            Error::Config(format!(
                "key {CONFIGMAP_KEY} not found in ConfigMap {}",
                tunnel_name
            ))
        })?;

    let mut config: Configuration = serde_yaml::from_str(config_str)
        .map_err(|e| Error::Config(format!("failed to parse cloudflared config: {e}")))?;

    // List all TunnelBindings for this tunnel
    let label_selector =
        format!("{TUNNEL_NAME_LABEL}={tunnel_name},{TUNNEL_KIND_LABEL}={tunnel_kind}");
    let lp = ListParams::default().labels(&label_selector);
    let binding_api: Api<TunnelBinding> = Api::all(k8s.clone());
    let binding_list = binding_api.list(&lp).await?;

    let mut bindings = binding_list.items;
    bindings.sort_by(|a, b| a.name_any().cmp(&b.name_any()));

    // Build ingress rules from all bindings
    let mut final_ingresses: Vec<UnvalidatedIngressRule> = Vec::new();
    for binding in &bindings {
        if let Some(status) = &binding.status {
            for (i, subject) in binding.subjects.iter().enumerate() {
                if i >= status.services.len() {
                    continue;
                }

                let target_service = if !subject.spec.target.is_empty() {
                    subject.spec.target.clone()
                } else {
                    status.services[i].target.clone()
                };

                let mut origin_req = OriginRequestConfig::default();
                origin_req.no_tls_verify = Some(subject.spec.no_tls_verify);
                origin_req.http2_origin = Some(subject.spec.http2_origin);
                if !subject.spec.proxy_address.is_empty() {
                    origin_req.proxy_address = Some(subject.spec.proxy_address.clone());
                }
                if subject.spec.proxy_port != 0 {
                    origin_req.proxy_port = Some(subject.spec.proxy_port);
                }
                if !subject.spec.proxy_type.is_empty() {
                    origin_req.proxy_type = Some(subject.spec.proxy_type.clone());
                }
                if !subject.spec.ca_pool.is_empty() {
                    origin_req.ca_pool =
                        Some(format!("/etc/cloudflared/certs/{}", subject.spec.ca_pool));
                }

                final_ingresses.push(UnvalidatedIngressRule {
                    hostname: Some(status.services[i].hostname.clone()),
                    service: target_service,
                    path: if subject.spec.path.is_empty() {
                        None
                    } else {
                        Some(subject.spec.path.clone())
                    },
                    origin_request: Some(origin_req),
                });
            }
        }
    }

    // Append catch-all
    final_ingresses.push(UnvalidatedIngressRule {
        hostname: None,
        service: fallback_target.to_string(),
        path: None,
        origin_request: None,
    });

    config.ingress = final_ingresses;

    // Push to CF edge (best-effort)
    let edge_rules: Vec<TunnelIngressRule> = config
        .ingress
        .iter()
        .map(|r| TunnelIngressRule {
            hostname: r.hostname.clone(),
            service: r.service.clone(),
            path: r.path.clone(),
        })
        .collect();
    if let Err(e) = cf_client.update_tunnel_configuration(&edge_rules).await {
        warn!(error = %e, "failed to sync configuration to cloudflare edge");
    }

    // Marshal new config
    let new_config_str = serde_yaml::to_string(&config)
        .map_err(|e| Error::Config(format!("failed to serialize config: {e}")))?;

    // Only update if content changed
    if config_str == &new_config_str {
        return Ok(());
    }

    let cm_api: Api<ConfigMap> = Api::namespaced(k8s.clone(), tunnel_ns);
    let cm_patch = serde_json::json!({
        "data": {
            CONFIGMAP_KEY: new_config_str
        }
    });
    cm_api
        .patch(
            tunnel_name,
            &PatchParams::apply("cloudflare-operator"),
            &Patch::Merge(&cm_patch),
        )
        .await?;

    // Update deployment checksum to trigger pod restart
    let new_checksum = compute_md5(&new_config_str);
    let deploy_api: Api<Deployment> = Api::namespaced(k8s.clone(), tunnel_ns);
    if let Ok(Some(dep)) = deploy_api.get_opt(tunnel_name).await {
        let current_checksum = dep
            .spec
            .as_ref()
            .and_then(|s| s.template.metadata.as_ref())
            .and_then(|m| m.annotations.as_ref())
            .and_then(|a| a.get(TUNNEL_CONFIG_CHECKSUM))
            .cloned()
            .unwrap_or_default();

        if current_checksum != new_checksum {
            let dep_patch = serde_json::json!({
                "spec": {
                    "template": {
                        "metadata": {
                            "annotations": {
                                TUNNEL_CONFIG_CHECKSUM: new_checksum
                            }
                        }
                    }
                }
            });
            deploy_api
                .patch(
                    tunnel_name,
                    &PatchParams::apply("cloudflare-operator"),
                    &Patch::Merge(&dep_patch),
                )
                .await?;
            info!(tunnel = %tunnel_name, "deployment checksum updated, pods will restart");
        }
    }

    info!(
        tunnel = %tunnel_name,
        binding_count = bindings.len(),
        "configmap updated from tunnel bindings"
    );
    Ok(())
}

// ── MD5 helper ──────────────────────────────────────────────────────────

fn compute_md5(input: &str) -> String {
    let mut hasher = Md5::new();
    hasher.update(input.as_bytes());
    let result = hasher.finalize();
    result.iter().map(|b| format!("{b:02x}")).collect()
}

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use k8s_openapi::api::apps::v1::{Deployment, DeploymentSpec};
use k8s_openapi::api::core::v1::{
    Affinity, Capabilities, ConfigMap, ConfigMapVolumeSource, Container, ContainerPort,
    HTTPGetAction, KeyToPath, NodeAffinity, NodeSelector,
    NodeSelectorRequirement, NodeSelectorTerm, PodSecurityContext, PodSpec, PodTemplateSpec,
    Probe, Secret, SeccompProfile, SecurityContext, SecretVolumeSource, Volume,
    VolumeMount,
};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{LabelSelector, ObjectMeta, OwnerReference};
use k8s_openapi::apimachinery::pkg::util::intstr::IntOrString;
use kube::api::{Api, ListParams, Patch, PatchParams};
use kube::runtime::controller::Action;
use kube::runtime::events::{Event, EventType};
use kube::{Resource, ResourceExt};
use md5::{Digest, Md5};
use serde::de::DeserializeOwned;
use serde::Serialize;
use tracing::{error, info, warn};

use crate::metrics::{RECONCILE_DURATION, RECONCILE_TOTAL};

use crate::cloudflare::client::CfClient;
use crate::cloudflare::types::TunnelCredentials;
use crate::config::cloudflared::{Configuration, OriginRequestConfig, UnvalidatedIngressRule};
use crate::crds::cluster_tunnel::ClusterTunnel;
use crate::crds::tunnel::{Tunnel, TunnelSpec, TunnelStatus};
use crate::crds::tunnel_binding::TunnelBinding;
use crate::crds::types::CloudflareDetails;
use crate::error::Error;

use super::context::Context;

const TUNNEL_FINALIZER: &str = "cfargotunnel.com/finalizer";
const CONFIGMAP_KEY: &str = "config.yaml";
const CREDENTIALS_JSON_FILENAME: &str = "credentials.json";
const TUNNEL_CONFIG_CHECKSUM: &str = "cfargotunnel.com/checksum";

const TUNNEL_LABEL: &str = "cfargotunnel.com/tunnel";
const TUNNEL_APP_LABEL: &str = "cfargotunnel.com/app";
const TUNNEL_ID_LABEL: &str = "cfargotunnel.com/id";
const TUNNEL_NAME_LABEL: &str = "cfargotunnel.com/name";
const TUNNEL_KIND_LABEL: &str = "cfargotunnel.com/kind";
const TUNNEL_DOMAIN_LABEL: &str = "cfargotunnel.com/domain";
const IS_CLUSTER_TUNNEL_LABEL: &str = "cfargotunnel.com/is-cluster-tunnel";

/// Trait abstracting Tunnel and ClusterTunnel so reconciliation logic is shared.
pub trait TunnelLike:
    Resource<DynamicType = ()> + Clone + std::fmt::Debug + Send + Sync + DeserializeOwned + Serialize + 'static
{
    fn tunnel_spec(&self) -> TunnelSpec;
    fn tunnel_status(&self) -> Option<&TunnelStatus>;
    fn set_tunnel_status(&mut self, status: TunnelStatus);
    fn resource_namespace(&self, default_ns: &str) -> String;
    fn is_cluster_scoped() -> bool;
    fn kind_str() -> &'static str;
}

impl TunnelLike for Tunnel {
    fn tunnel_spec(&self) -> TunnelSpec {
        self.spec.clone()
    }

    fn tunnel_status(&self) -> Option<&TunnelStatus> {
        self.status.as_ref()
    }

    fn set_tunnel_status(&mut self, status: TunnelStatus) {
        self.status = Some(status);
    }

    fn resource_namespace(&self, _default_ns: &str) -> String {
        self.namespace().unwrap_or_default()
    }

    fn is_cluster_scoped() -> bool {
        false
    }

    fn kind_str() -> &'static str {
        "Tunnel"
    }
}

impl TunnelLike for ClusterTunnel {
    fn tunnel_spec(&self) -> TunnelSpec {
        let cs = &self.spec;
        TunnelSpec {
            deploy_patch: cs.deploy_patch.clone(),
            no_tls_verify: cs.no_tls_verify,
            origin_ca_pool: cs.origin_ca_pool.clone(),
            protocol: cs.protocol.clone(),
            fallback_target: cs.fallback_target.clone(),
            cloudflare: cs.cloudflare.clone(),
            existing_tunnel: cs.existing_tunnel.clone(),
            new_tunnel: cs.new_tunnel.clone(),
            cloudflared_image: cs.cloudflared_image.clone(),
        }
    }

    fn tunnel_status(&self) -> Option<&TunnelStatus> {
        self.status.as_ref()
    }

    fn set_tunnel_status(&mut self, status: TunnelStatus) {
        self.status = Some(status);
    }

    fn resource_namespace(&self, default_ns: &str) -> String {
        default_ns.to_string()
    }

    fn is_cluster_scoped() -> bool {
        true
    }

    fn kind_str() -> &'static str {
        "ClusterTunnel"
    }
}

/// Main reconcile function for both Tunnel and ClusterTunnel.
pub async fn reconcile_tunnel<T: TunnelLike>(
    obj: Arc<T>,
    ctx: Arc<Context>,
) -> Result<Action, Error> {
    let start = std::time::Instant::now();
    let controller_label = T::kind_str().to_lowercase();
    let result = reconcile_tunnel_inner::<T>(obj, ctx).await;
    let elapsed = start.elapsed().as_secs_f64();
    RECONCILE_DURATION
        .with_label_values(&[&controller_label])
        .observe(elapsed);
    let result_label = if result.is_ok() { "success" } else { "error" };
    RECONCILE_TOTAL
        .with_label_values(&[&controller_label, result_label])
        .inc();
    result
}

async fn reconcile_tunnel_inner<T: TunnelLike>(
    obj: Arc<T>,
    ctx: Arc<Context>,
) -> Result<Action, Error> {
    let k8s = &ctx.client;
    let ns = obj.resource_namespace(&ctx.cluster_resource_namespace);
    let name = obj.name_any();
    let spec = obj.tunnel_spec();
    let status = obj.tunnel_status().cloned().unwrap_or_default();
    let recorder = ctx.recorder();
    let obj_ref = obj.object_ref(&());

    info!(name = %name, ns = %ns, kind = T::kind_str(), "reconciling tunnel");
    recorder
        .publish(
            &Event {
                type_: EventType::Normal,
                reason: "Reconciling".into(),
                note: Some(format!("Reconciling {} {name}", T::kind_str())),
                action: "Reconciling".into(),
                secondary: None,
            },
            &obj_ref,
        )
        .await
        .ok();

    // 1. Read CF credentials from the referenced Secret
    let secrets_api: Api<Secret> = Api::namespaced(k8s.clone(), &ns);
    let cf_secret = secrets_api.get(&spec.cloudflare.secret).await.map_err(|e| {
        error!(secret = %spec.cloudflare.secret, ns = %ns, error = %e, "failed to read cloudflare secret");
        e
    })?;

    // 2. Build CfClient from the secret data
    let mut cf_client = build_cf_client(&spec.cloudflare, &cf_secret, ctx.cloudflare_api_base_url.as_deref())?;
    cf_client.domain = spec.cloudflare.domain.clone();

    // Pre-populate validated IDs from existing status so validate_all can short-circuit
    cf_client.account_id = status.account_id.clone();
    cf_client.tunnel_id = status.tunnel_id.clone();
    cf_client.tunnel_name = status.tunnel_name.clone();
    cf_client.zone_id = status.zone_id.clone();

    let ok_new = spec.new_tunnel.is_some();
    let ok_existing = spec.existing_tunnel.is_some();

    if ok_new == ok_existing {
        return Err(Error::Config(
            "ExistingTunnel and NewTunnel cannot be both empty and are mutually exclusive".into(),
        ));
    }

    let mut tunnel_creds = String::new();

    if ok_existing {
        // ── Existing tunnel ─────────────────────────────────────────────
        let existing = spec.existing_tunnel.as_ref().unwrap();
        cf_client.tunnel_name = existing.name.clone();
        cf_client.tunnel_id = existing.id.clone();

        let secret_data = cf_secret.data.as_ref().ok_or_else(|| {
            Error::MissingField("secret has no data".into())
        })?;

        let cred_file_key = &spec.cloudflare.cloudflare_tunnel_credential_file;
        let cred_secret_key = &spec.cloudflare.cloudflare_tunnel_credential_secret;

        if let Some(cred_file) = secret_data.get(cred_file_key) {
            tunnel_creds = String::from_utf8_lossy(&cred_file.0).to_string();
        } else if let Some(cred_secret) = secret_data.get(cred_secret_key) {
            let secret_str = String::from_utf8_lossy(&cred_secret.0);
            let creds = TunnelCredentials {
                account_tag: cf_client.account_id.clone(),
                tunnel_id: cf_client.tunnel_id.clone(),
                tunnel_name: cf_client.tunnel_name.clone(),
                tunnel_secret: secret_str.to_string(),
            };
            tunnel_creds = serde_json::to_string(&creds)?;
        } else {
            return Err(Error::MissingField(format!(
                "neither {cred_file_key} nor {cred_secret_key} found in secret {}",
                spec.cloudflare.secret
            )));
        }
    } else {
        // ── New tunnel ──────────────────────────────────────────────────
        let is_deleting = obj
            .meta()
            .deletion_timestamp
            .is_some();

        if is_deleting {
            // Cleanup path
            let has_finalizer = obj
                .meta()
                .finalizers
                .as_ref()
                .map_or(false, |f| f.contains(&TUNNEL_FINALIZER.to_string()));

            if has_finalizer {
                info!(name = %name, "starting tunnel deletion cycle");
                recorder
                    .publish(
                        &Event {
                            type_: EventType::Normal,
                            reason: "Deleting".into(),
                            note: Some(format!("Starting deletion of tunnel {name}")),
                            action: "Deleting".into(),
                            secondary: None,
                        },
                        &obj_ref,
                    )
                    .await
                    .ok();

                // Scale deployment to 0 before deleting tunnel
                let deploy_api: Api<Deployment> = Api::namespaced(k8s.clone(), &ns);
                let scale_needed = match deploy_api.get(&name).await {
                    Ok(dep) => dep
                        .spec
                        .as_ref()
                        .and_then(|s| s.replicas)
                        .unwrap_or(1)
                        != 0,
                    Err(_) => false,
                };

                if scale_needed {
                    info!(name = %name, "scaling down cloudflared deployment");
                    recorder
                        .publish(
                            &Event {
                                type_: EventType::Normal,
                                reason: "Scaling".into(),
                                note: Some("Scaling cloudflared deployment to 0 for deletion".into()),
                                action: "Scaling".into(),
                                secondary: None,
                            },
                            &obj_ref,
                        )
                        .await
                        .ok();
                    let patch = serde_json::json!({
                        "spec": { "replicas": 0 }
                    });
                    deploy_api
                        .patch(&name, &PatchParams::apply("cloudflare-operator"), &Patch::Merge(&patch))
                        .await?;
                    recorder
                        .publish(
                            &Event {
                                type_: EventType::Normal,
                                reason: "Scaled".into(),
                                note: Some("Deployment scaled to 0".into()),
                                action: "Scaling".into(),
                                secondary: None,
                            },
                            &obj_ref,
                        )
                        .await
                        .ok();
                    return Ok(Action::requeue(Duration::from_secs(5)));
                }

                // Clean up DNS records for all bindings referencing this tunnel
                cleanup_dns_records(k8s, &mut cf_client, &name, T::kind_str(), &ns, T::is_cluster_scoped()).await;
                recorder
                    .publish(
                        &Event {
                            type_: EventType::Normal,
                            reason: "DNSCleaned".into(),
                            note: Some("DNS records cleaned up for tunnel bindings".into()),
                            action: "Deleting".into(),
                            secondary: None,
                        },
                        &obj_ref,
                    )
                    .await
                    .ok();

                // Clear edge configuration (best-effort)
                if let Err(e) = cf_client.clear_tunnel_configuration().await {
                    warn!(name = %name, error = %e, "failed to clear edge configuration, proceeding with deletion");
                }

                // Delete tunnel on Cloudflare
                match cf_client.delete_tunnel().await {
                    Ok(()) => {
                        info!(name = %name, tunnel_id = %cf_client.tunnel_id, "tunnel deleted on cloudflare");
                        recorder
                            .publish(
                                &Event {
                                    type_: EventType::Normal,
                                    reason: "Deleted".into(),
                                    note: Some(format!(
                                        "Tunnel {} deleted on Cloudflare",
                                        cf_client.tunnel_id
                                    )),
                                    action: "Deleting".into(),
                                    secondary: None,
                                },
                                &obj_ref,
                            )
                            .await
                            .ok();
                    }
                    Err(e) => {
                        recorder
                            .publish(
                                &Event {
                                    type_: EventType::Warning,
                                    reason: "FailedDeleting".into(),
                                    note: Some(format!("Failed to delete tunnel on Cloudflare: {e}")),
                                    action: "Deleting".into(),
                                    secondary: None,
                                },
                                &obj_ref,
                            )
                            .await
                            .ok();
                        return Err(e);
                    }
                }

                // Remove finalizer
                let patch = serde_json::json!({
                    "metadata": {
                        "finalizers": obj.meta().finalizers.as_ref()
                            .map(|f| f.iter().filter(|fin| fin.as_str() != TUNNEL_FINALIZER).cloned().collect::<Vec<_>>())
                            .unwrap_or_default()
                    }
                });
                patch_tunnel(k8s, &name, &ns, T::is_cluster_scoped(), &patch).await?;
                recorder
                    .publish(
                        &Event {
                            type_: EventType::Normal,
                            reason: "FinalizerUnset".into(),
                            note: Some("Finalizer removed".into()),
                            action: "Deleting".into(),
                            secondary: None,
                        },
                        &obj_ref,
                    )
                    .await
                    .ok();
            }
            return Ok(Action::await_change());
        }

        // Not deleting: create tunnel if needed
        if status.tunnel_id.is_empty() {
            let tunnel_name = spec.new_tunnel.as_ref().unwrap().name.clone();
            let tn = if tunnel_name.is_empty() { name.clone() } else { tunnel_name };

            // Validate account first so we have account_id for create_tunnel
            cf_client
                .validate_account(&spec.cloudflare.account_id, &spec.cloudflare.account_name)
                .await?;

            match cf_client.create_tunnel(&cf_client.account_id.clone(), &tn).await {
                Ok((tunnel_id, creds_json)) => {
                    info!(name = %name, tunnel_id = %tunnel_id, "tunnel created on cloudflare");
                    recorder
                        .publish(
                            &Event {
                                type_: EventType::Normal,
                                reason: "Created".into(),
                                note: Some(format!("Tunnel {tunnel_id} created on Cloudflare")),
                                action: "Creating".into(),
                                secondary: None,
                            },
                            &obj_ref,
                        )
                        .await
                        .ok();
                    tunnel_creds = creds_json;
                }
                Err(e) => {
                    recorder
                        .publish(
                            &Event {
                                type_: EventType::Warning,
                                reason: "FailedCreate".into(),
                                note: Some(format!("Failed to create tunnel on Cloudflare: {e}")),
                                action: "Creating".into(),
                                secondary: None,
                            },
                            &obj_ref,
                        )
                        .await
                        .ok();
                    return Err(e);
                }
            }
        } else {
            // Tunnel already created, read creds from existing managed secret
            match secrets_api.get(&name).await {
                Ok(sec) => {
                    if let Some(data) = &sec.data {
                        if let Some(creds) = data.get(CREDENTIALS_JSON_FILENAME) {
                            tunnel_creds = String::from_utf8_lossy(&creds.0).to_string();
                        }
                    }
                }
                Err(e) => {
                    error!(name = %name, error = %e, "could not read existing tunnel credentials secret");
                }
            }
        }

        // Add finalizer if not present
        let has_finalizer = obj
            .meta()
            .finalizers
            .as_ref()
            .map_or(false, |f| f.contains(&TUNNEL_FINALIZER.to_string()));

        if !has_finalizer {
            let mut finalizers = obj.meta().finalizers.clone().unwrap_or_default();
            finalizers.push(TUNNEL_FINALIZER.to_string());
            let patch = serde_json::json!({
                "metadata": {
                    "finalizers": finalizers
                }
            });
            patch_tunnel(k8s, &name, &ns, T::is_cluster_scoped(), &patch).await?;
            info!(name = %name, "finalizer added");
            recorder
                .publish(
                    &Event {
                        type_: EventType::Normal,
                        reason: "FinalizerSet".into(),
                        note: Some("Finalizer added to tunnel".into()),
                        action: "Reconciling".into(),
                        secondary: None,
                    },
                    &obj_ref,
                )
                .await
                .ok();
        }
    }

    // 3. Validate all CF resources (account, tunnel, zone)
    cf_client
        .validate_all(
            &spec.cloudflare.account_id,
            &spec.cloudflare.account_name,
            &cf_client.tunnel_id.clone(),
            &cf_client.tunnel_name.clone(),
            &spec.cloudflare.domain,
        )
        .await?;

    // 4. Update labels on the tunnel resource
    let labels = labels_for_tunnel(&name, &cf_client, &spec.cloudflare.domain, T::is_cluster_scoped());
    let label_patch = serde_json::json!({
        "metadata": { "labels": labels }
    });
    patch_tunnel(k8s, &name, &ns, T::is_cluster_scoped(), &label_patch).await?;

    // 5. Update status
    let new_status = TunnelStatus {
        tunnel_id: cf_client.tunnel_id.clone(),
        tunnel_name: cf_client.tunnel_name.clone(),
        account_id: cf_client.account_id.clone(),
        zone_id: cf_client.zone_id.clone(),
    };
    let status_patch = serde_json::json!({
        "status": new_status
    });
    match patch_tunnel_status(k8s, &name, &ns, T::is_cluster_scoped(), &status_patch).await {
        Ok(()) => {
            info!(name = %name, tunnel_id = %cf_client.tunnel_id, "tunnel status updated");
            recorder
                .publish(
                    &Event {
                        type_: EventType::Normal,
                        reason: "StatusUpdated".into(),
                        note: Some(format!(
                            "Status updated: tunnel_id={}, zone_id={}",
                            cf_client.tunnel_id, cf_client.zone_id
                        )),
                        action: "Reconciling".into(),
                        secondary: None,
                    },
                    &obj_ref,
                )
                .await
                .ok();
        }
        Err(e) => {
            recorder
                .publish(
                    &Event {
                        type_: EventType::Warning,
                        reason: "FailedStatusSet".into(),
                        note: Some(format!("Failed to update tunnel status: {e}")),
                        action: "Reconciling".into(),
                        secondary: None,
                    },
                    &obj_ref,
                )
                .await
                .ok();
            return Err(e);
        }
    }

    // 6. Create/update managed Secret with tunnel credentials
    if !tunnel_creds.is_empty() {
        let oref = build_owner_ref::<T>(&obj);
        let managed_secret = build_credentials_secret(&name, &ns, &tunnel_creds, &labels, &oref);
        let secret_api: Api<Secret> = Api::namespaced(k8s.clone(), &ns);
        secret_api
            .patch(
                &name,
                &PatchParams::apply("cloudflare-operator"),
                &Patch::Apply(managed_secret),
            )
            .await?;
        info!(name = %name, "credentials secret applied");
        recorder
            .publish(
                &Event {
                    type_: EventType::Normal,
                    reason: "SecretApplied".into(),
                    note: Some("Tunnel credentials secret applied".into()),
                    action: "Reconciling".into(),
                    secondary: None,
                },
                &obj_ref,
            )
            .await
            .ok();
    } else {
        warn!(name = %name, "empty tunnel credentials, skipping secret update");
    }

    // 7. Build and apply ConfigMap with cloudflared config
    let oref = build_owner_ref::<T>(&obj);
    let initial_config = build_cloudflared_config(
        &cf_client.tunnel_id,
        &spec.fallback_target,
        spec.no_tls_verify,
        &spec.origin_ca_pool,
    );
    let config_yaml = serde_yaml::to_string(&initial_config)
        .map_err(|e| Error::Config(format!("failed to serialize cloudflared config: {e}")))?;

    let cm = build_config_map(&name, &ns, &config_yaml, &labels, &oref);
    let cm_api: Api<ConfigMap> = Api::namespaced(k8s.clone(), &ns);

    // Use merge patch so we don't overwrite ingress rules added by TunnelBinding reconciler
    let existing_cm = cm_api.get_opt(&name).await?;
    let applied_config_yaml = if let Some(existing) = &existing_cm {
        existing
            .data
            .as_ref()
            .and_then(|d| d.get(CONFIGMAP_KEY))
            .cloned()
            .unwrap_or(config_yaml.clone())
    } else {
        config_yaml.clone()
    };

    cm_api
        .patch(
            &name,
            &PatchParams::apply("cloudflare-operator"),
            &Patch::Apply(cm),
        )
        .await?;
    info!(name = %name, "configmap applied");
    recorder
        .publish(
            &Event {
                type_: EventType::Normal,
                reason: "ConfigMapApplied".into(),
                note: Some("Cloudflared configuration ConfigMap applied".into()),
                action: "Reconciling".into(),
                secondary: None,
            },
            &obj_ref,
        )
        .await
        .ok();

    // 8. Build and apply Deployment
    let config_hash = compute_md5(&applied_config_yaml);
    let effective_image = spec
        .cloudflared_image
        .as_deref()
        .filter(|s| !s.is_empty())
        .unwrap_or(&ctx.cloudflared_image);
    let dep = build_deployment(
        &name,
        &ns,
        effective_image,
        &spec.protocol,
        &spec.origin_ca_pool,
        &config_hash,
        &labels,
        &oref,
    );
    let deploy_api: Api<Deployment> = Api::namespaced(k8s.clone(), &ns);
    deploy_api
        .patch(
            &name,
            &PatchParams::apply("cloudflare-operator"),
            &Patch::Apply(dep),
        )
        .await?;
    info!(name = %name, "deployment applied");
    recorder
        .publish(
            &Event {
                type_: EventType::Normal,
                reason: "DeploymentApplied".into(),
                note: Some("Cloudflared Deployment applied".into()),
                action: "Reconciling".into(),
                secondary: None,
            },
            &obj_ref,
        )
        .await
        .ok();

    // Apply deploy_patch as a second SSA pass with a different field manager
    let deploy_patch_str = &spec.deploy_patch;
    if deploy_patch_str != "{}" && !deploy_patch_str.is_empty() {
        let patch_value: serde_json::Value = serde_json::from_str(deploy_patch_str)?;
        let patch_obj = serde_json::json!({
            "apiVersion": "apps/v1",
            "kind": "Deployment",
            "metadata": {"name": name, "namespace": ns},
            "spec": patch_value
        });
        let pp2 = PatchParams::apply("cloudflare-operator-patch");
        deploy_api
            .patch(&name, &pp2, &Patch::Apply(&patch_obj))
            .await?;
        info!(name = %name, "deploy_patch applied");
    }

    // 9. Rebuild ConfigMap ingress from current TunnelBindings
    rebuild_tunnel_config(k8s, &name, T::kind_str(), &ns, &spec.fallback_target, &cf_client).await?;

    Ok(Action::requeue(Duration::from_secs(300)))
}

/// Error policy: exponential backoff on errors.
pub fn error_policy<T: TunnelLike>(_obj: Arc<T>, error: &Error, _ctx: Arc<Context>) -> Action {
    error!(error = %error, "reconciliation error, will retry");
    Action::requeue(Duration::from_secs(15))
}

// ── Helper: build CfClient from K8s Secret ──────────────────────────────

fn build_cf_client(
    cf: &CloudflareDetails,
    secret: &Secret,
    base_url: Option<&str>,
) -> Result<CfClient, Error> {
    let data = secret.data.as_ref().ok_or_else(|| {
        Error::MissingField("cloudflare secret has no data".into())
    })?;

    let api_token = data.get(&cf.cloudflare_api_token).map(|b| String::from_utf8_lossy(&b.0).to_string());
    let api_key = data.get(&cf.cloudflare_api_key).map(|b| String::from_utf8_lossy(&b.0).to_string());

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

// ── Helper: build labels ────────────────────────────────────────────────

fn labels_for_tunnel(
    name: &str,
    cf_client: &CfClient,
    domain: &str,
    is_cluster: bool,
) -> BTreeMap<String, String> {
    let mut labels = BTreeMap::new();
    labels.insert(TUNNEL_LABEL.to_string(), name.to_string());
    labels.insert(TUNNEL_APP_LABEL.to_string(), "cloudflared".to_string());
    labels.insert(TUNNEL_ID_LABEL.to_string(), cf_client.tunnel_id.clone());
    labels.insert(TUNNEL_NAME_LABEL.to_string(), cf_client.tunnel_name.clone());
    labels.insert(TUNNEL_DOMAIN_LABEL.to_string(), domain.to_string());
    labels.insert(
        IS_CLUSTER_TUNNEL_LABEL.to_string(),
        if is_cluster { "true" } else { "false" }.to_string(),
    );
    labels
}

// ── Helper: build owner reference ───────────────────────────────────────

fn build_owner_ref<T: TunnelLike>(obj: &T) -> OwnerReference {
    OwnerReference {
        api_version: T::api_version(&()).to_string(),
        kind: T::kind(&()).to_string(),
        name: obj.name_any(),
        uid: obj.uid().unwrap_or_default(),
        controller: Some(true),
        block_owner_deletion: Some(true),
    }
}

// ── Helper: patch resource ──────────────────────────────────────────────

async fn patch_tunnel(
    k8s: &kube::Client,
    name: &str,
    ns: &str,
    is_cluster: bool,
    patch: &serde_json::Value,
) -> Result<(), Error> {
    let pp = PatchParams::apply("cloudflare-operator");
    if is_cluster {
        let api: Api<ClusterTunnel> = Api::all(k8s.clone());
        api.patch(name, &pp, &Patch::Merge(patch)).await?;
    } else {
        let api: Api<Tunnel> = Api::namespaced(k8s.clone(), ns);
        api.patch(name, &pp, &Patch::Merge(patch)).await?;
    }
    Ok(())
}

async fn patch_tunnel_status(
    k8s: &kube::Client,
    name: &str,
    ns: &str,
    is_cluster: bool,
    patch: &serde_json::Value,
) -> Result<(), Error> {
    let pp = PatchParams::apply("cloudflare-operator");
    if is_cluster {
        let api: Api<ClusterTunnel> = Api::all(k8s.clone());
        api.patch_status(name, &pp, &Patch::Merge(patch)).await?;
    } else {
        let api: Api<Tunnel> = Api::namespaced(k8s.clone(), ns);
        api.patch_status(name, &pp, &Patch::Merge(patch)).await?;
    }
    Ok(())
}

// ── Helper: build credentials Secret ────────────────────────────────────

fn build_credentials_secret(
    name: &str,
    ns: &str,
    creds: &str,
    labels: &BTreeMap<String, String>,
    oref: &OwnerReference,
) -> Secret {
    Secret {
        metadata: ObjectMeta {
            name: Some(name.to_string()),
            namespace: Some(ns.to_string()),
            labels: Some(labels.clone()),
            owner_references: Some(vec![oref.clone()]),
            ..Default::default()
        },
        string_data: Some(BTreeMap::from([(
            CREDENTIALS_JSON_FILENAME.to_string(),
            creds.to_string(),
        )])),
        ..Default::default()
    }
}

// ── Helper: build cloudflared Configuration ─────────────────────────────

fn build_cloudflared_config(
    tunnel_id: &str,
    fallback_target: &str,
    no_tls_verify: bool,
    origin_ca_pool: &str,
) -> Configuration {
    let mut origin_request = OriginRequestConfig {
        no_tls_verify: Some(no_tls_verify),
        ..Default::default()
    };
    if !origin_ca_pool.is_empty() {
        origin_request.ca_pool = Some("/etc/cloudflared/certs/tls.crt".to_string());
    }

    Configuration {
        tunnel: tunnel_id.to_string(),
        credentials_file: "/etc/cloudflared/creds/credentials.json".to_string(),
        metrics: Some("0.0.0.0:2000".to_string()),
        no_auto_update: Some(true),
        origin_request: Some(origin_request),
        ingress: vec![UnvalidatedIngressRule {
            hostname: None,
            service: fallback_target.to_string(),
            path: None,
            origin_request: None,
        }],
        warp_routing: None,
    }
}

// ── Helper: build ConfigMap ─────────────────────────────────────────────

fn build_config_map(
    name: &str,
    ns: &str,
    config_yaml: &str,
    labels: &BTreeMap<String, String>,
    oref: &OwnerReference,
) -> ConfigMap {
    ConfigMap {
        metadata: ObjectMeta {
            name: Some(name.to_string()),
            namespace: Some(ns.to_string()),
            labels: Some(labels.clone()),
            owner_references: Some(vec![oref.clone()]),
            ..Default::default()
        },
        data: Some(BTreeMap::from([(
            CONFIGMAP_KEY.to_string(),
            config_yaml.to_string(),
        )])),
        ..Default::default()
    }
}

// ── Helper: build Deployment ────────────────────────────────────────────

fn build_deployment(
    name: &str,
    ns: &str,
    image: &str,
    protocol: &str,
    origin_ca_pool: &str,
    config_checksum: &str,
    labels: &BTreeMap<String, String>,
    oref: &OwnerReference,
) -> Deployment {
    let args = vec![
        "tunnel".to_string(),
        "--protocol".to_string(),
        protocol.to_string(),
        "--config".to_string(),
        "/etc/cloudflared/config/config.yaml".to_string(),
        "--metrics".to_string(),
        "0.0.0.0:2000".to_string(),
        "run".to_string(),
    ];

    let default_mode: i32 = 420; // 0644

    let mut volumes = vec![
        Volume {
            name: "creds".to_string(),
            secret: Some(SecretVolumeSource {
                secret_name: Some(name.to_string()),
                default_mode: Some(default_mode),
                ..Default::default()
            }),
            ..Default::default()
        },
        Volume {
            name: "config".to_string(),
            config_map: Some(ConfigMapVolumeSource {
                name: name.to_string(),
                items: Some(vec![KeyToPath {
                    key: CONFIGMAP_KEY.to_string(),
                    path: CONFIGMAP_KEY.to_string(),
                    ..Default::default()
                }]),
                default_mode: Some(default_mode),
                ..Default::default()
            }),
            ..Default::default()
        },
    ];

    let mut volume_mounts = vec![
        VolumeMount {
            name: "config".to_string(),
            mount_path: "/etc/cloudflared/config".to_string(),
            read_only: Some(true),
            ..Default::default()
        },
        VolumeMount {
            name: "creds".to_string(),
            mount_path: "/etc/cloudflared/creds".to_string(),
            read_only: Some(true),
            ..Default::default()
        },
    ];

    if !origin_ca_pool.is_empty() {
        volumes.push(Volume {
            name: "certs".to_string(),
            secret: Some(SecretVolumeSource {
                secret_name: Some(origin_ca_pool.to_string()),
                default_mode: Some(default_mode),
                ..Default::default()
            }),
            ..Default::default()
        });
        volume_mounts.push(VolumeMount {
            name: "certs".to_string(),
            mount_path: "/etc/cloudflared/certs".to_string(),
            read_only: Some(true),
            ..Default::default()
        });
    }

    let mut annotations = BTreeMap::new();
    annotations.insert(TUNNEL_CONFIG_CHECKSUM.to_string(), config_checksum.to_string());

    Deployment {
        metadata: ObjectMeta {
            name: Some(name.to_string()),
            namespace: Some(ns.to_string()),
            labels: Some(labels.clone()),
            owner_references: Some(vec![oref.clone()]),
            ..Default::default()
        },
        spec: Some(DeploymentSpec {
            selector: LabelSelector {
                match_labels: Some(labels.clone()),
                ..Default::default()
            },
            template: PodTemplateSpec {
                metadata: Some(ObjectMeta {
                    labels: Some(labels.clone()),
                    annotations: Some(annotations),
                    ..Default::default()
                }),
                spec: Some(PodSpec {
                    security_context: Some(PodSecurityContext {
                        run_as_non_root: Some(true),
                        seccomp_profile: Some(SeccompProfile {
                            type_: "RuntimeDefault".to_string(),
                            ..Default::default()
                        }),
                        ..Default::default()
                    }),
                    containers: vec![Container {
                        name: "cloudflared".to_string(),
                        image: Some(image.to_string()),
                        args: Some(args),
                        liveness_probe: Some(Probe {
                            http_get: Some(HTTPGetAction {
                                path: Some("/ready".to_string()),
                                port: IntOrString::Int(2000),
                                ..Default::default()
                            }),
                            failure_threshold: Some(1),
                            initial_delay_seconds: Some(10),
                            period_seconds: Some(10),
                            ..Default::default()
                        }),
                        ports: Some(vec![ContainerPort {
                            name: Some("metrics".to_string()),
                            container_port: 2000,
                            protocol: Some("TCP".to_string()),
                            ..Default::default()
                        }]),
                        volume_mounts: Some(volume_mounts),
                        security_context: Some(SecurityContext {
                            allow_privilege_escalation: Some(false),
                            read_only_root_filesystem: Some(true),
                            run_as_user: Some(1002),
                            capabilities: Some(Capabilities {
                                drop: Some(vec!["ALL".to_string()]),
                                ..Default::default()
                            }),
                            ..Default::default()
                        }),
                        ..Default::default()
                    }],
                    volumes: Some(volumes),
                    affinity: Some(Affinity {
                        node_affinity: Some(NodeAffinity {
                            required_during_scheduling_ignored_during_execution: Some(NodeSelector {
                                node_selector_terms: vec![NodeSelectorTerm {
                                    match_expressions: Some(vec![
                                        NodeSelectorRequirement {
                                            key: "kubernetes.io/arch".to_string(),
                                            operator: "In".to_string(),
                                            values: Some(vec![
                                                "amd64".to_string(),
                                                "arm64".to_string(),
                                            ]),
                                        },
                                        NodeSelectorRequirement {
                                            key: "kubernetes.io/os".to_string(),
                                            operator: "In".to_string(),
                                            values: Some(vec!["linux".to_string()]),
                                        },
                                    ]),
                                    ..Default::default()
                                }],
                            }),
                            ..Default::default()
                        }),
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
            },
            ..Default::default()
        }),
        ..Default::default()
    }
}

// ── Helper: cleanup DNS records during tunnel deletion ──────────────────

async fn cleanup_dns_records(
    k8s: &kube::Client,
    cf_client: &mut CfClient,
    tunnel_name: &str,
    tunnel_kind: &str,
    ns: &str,
    is_cluster: bool,
) {
    // Validate zone before trying DNS operations
    if cf_client.zone_id.is_empty() {
        if let Err(e) = cf_client.validate_zone(&cf_client.domain.clone()).await {
            warn!(error = %e, "cannot validate zone for DNS cleanup, skipping");
            return;
        }
    }

    let label_selector = format!(
        "{TUNNEL_NAME_LABEL}={tunnel_name},{TUNNEL_KIND_LABEL}={tunnel_kind}"
    );
    let lp = ListParams::default().labels(&label_selector);

    let bindings: Vec<TunnelBinding> = if is_cluster {
        let api: Api<TunnelBinding> = Api::all(k8s.clone());
        match api.list(&lp).await {
            Ok(list) => list.items,
            Err(e) => {
                warn!(error = %e, "failed to list TunnelBindings for DNS cleanup");
                return;
            }
        }
    } else {
        let api: Api<TunnelBinding> = Api::namespaced(k8s.clone(), ns);
        match api.list(&lp).await {
            Ok(list) => list.items,
            Err(e) => {
                warn!(error = %e, "failed to list TunnelBindings for DNS cleanup");
                return;
            }
        }
    };

    let mut seen = std::collections::HashSet::new();
    for binding in &bindings {
        if let Some(status) = &binding.status {
            for svc in &status.services {
                if svc.hostname.is_empty() || !seen.insert(svc.hostname.clone()) {
                    continue;
                }
                if let Err(e) = delete_dns_for_hostname(cf_client, &svc.hostname).await {
                    warn!(hostname = %svc.hostname, error = %e, "failed to delete DNS records for hostname");
                }
            }
        }
    }
    info!(tunnel = %tunnel_name, hostname_count = seen.len(), "DNS cleanup complete");
}

async fn delete_dns_for_hostname(cf_client: &CfClient, hostname: &str) -> Result<(), Error> {
    let (txt_id, txt_data, can_use) = match cf_client.get_managed_dns_txt(hostname).await {
        Ok(result) => result,
        Err(_) => return Ok(()),
    };

    if !can_use {
        return Ok(());
    }

    if !txt_data.dns_id.is_empty() {
        if let Err(e) = cf_client.delete_dns_id(hostname, &txt_data.dns_id, true).await {
            warn!(hostname = %hostname, error = %e, "failed to delete CNAME record");
        } else {
            info!(hostname = %hostname, "deleted DNS CNAME record");
        }
    }

    if !txt_id.is_empty() {
        if let Err(e) = cf_client.delete_dns_id(hostname, &txt_id, true).await {
            warn!(hostname = %hostname, error = %e, "failed to delete TXT record");
        } else {
            info!(hostname = %hostname, "deleted DNS TXT record");
        }
    }

    Ok(())
}

// ── Helper: rebuild config from TunnelBindings ──────────────────────────

async fn rebuild_tunnel_config(
    k8s: &kube::Client,
    tunnel_name: &str,
    tunnel_kind: &str,
    ns: &str,
    fallback_target: &str,
    cf_client: &CfClient,
) -> Result<(), Error> {
    let cm_api: Api<ConfigMap> = Api::namespaced(k8s.clone(), ns);
    let cm = match cm_api.get_opt(tunnel_name).await? {
        Some(cm) => cm,
        None => return Ok(()),
    };

    let config_str = cm
        .data
        .as_ref()
        .and_then(|d| d.get(CONFIGMAP_KEY))
        .ok_or_else(|| Error::Config(format!("key {CONFIGMAP_KEY} not found in ConfigMap")))?;

    let mut config: Configuration = serde_yaml::from_str(config_str)
        .map_err(|e| Error::Config(format!("failed to parse cloudflared config: {e}")))?;

    // List TunnelBindings for this tunnel
    let label_selector = format!(
        "{TUNNEL_NAME_LABEL}={tunnel_name},{TUNNEL_KIND_LABEL}={tunnel_kind}"
    );
    let lp = ListParams::default().labels(&label_selector);
    let binding_api: Api<TunnelBinding> = Api::all(k8s.clone());
    let binding_list = binding_api.list(&lp).await?;

    let mut bindings = binding_list.items;
    bindings.sort_by(|a, b| a.name_any().cmp(&b.name_any()));

    // Build ingress rules from all current bindings
    let mut final_ingresses: Vec<UnvalidatedIngressRule> = Vec::new();
    for binding in &bindings {
        if let Some(status) = &binding.status {
            for (i, subject) in binding.subjects.iter().enumerate() {
                if i >= status.services.len() {
                    continue;
                }
                let target_service = if subject.spec.target.is_empty() {
                    status.services[i].target.clone()
                } else {
                    subject.spec.target.clone()
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
                    origin_req.ca_pool = Some(format!("/etc/cloudflared/certs/{}", subject.spec.ca_pool));
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

    // Add catch-all fallback
    final_ingresses.push(UnvalidatedIngressRule {
        hostname: None,
        service: fallback_target.to_string(),
        path: None,
        origin_request: None,
    });

    config.ingress = final_ingresses;

    // Push to CF edge (best-effort)
    let edge_rules: Vec<crate::cloudflare::types::TunnelIngressRule> = config
        .ingress
        .iter()
        .map(|r| crate::cloudflare::types::TunnelIngressRule {
            hostname: r.hostname.clone(),
            service: r.service.clone(),
            path: r.path.clone(),
        })
        .collect();
    if let Err(e) = cf_client.update_tunnel_configuration(&edge_rules).await {
        warn!(error = %e, "failed to sync configuration to cloudflare edge during rebuild");
    }

    // Marshal new config
    let new_config_str = serde_yaml::to_string(&config)
        .map_err(|e| Error::Config(format!("failed to serialize config: {e}")))?;

    // Only update if content changed
    if config_str == &new_config_str {
        return Ok(());
    }

    let cm_patch = serde_json::json!({
        "data": {
            CONFIGMAP_KEY: new_config_str
        }
    });
    cm_api
        .patch(tunnel_name, &PatchParams::apply("cloudflare-operator"), &Patch::Merge(&cm_patch))
        .await?;

    // Update deployment checksum to trigger pod restart
    let new_checksum = compute_md5(&new_config_str);
    let deploy_api: Api<Deployment> = Api::namespaced(k8s.clone(), ns);
    match deploy_api.get_opt(tunnel_name).await? {
        Some(dep) => {
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
                    .patch(tunnel_name, &PatchParams::apply("cloudflare-operator"), &Patch::Merge(&dep_patch))
                    .await?;
            }
        }
        None => {}
    }

    info!(tunnel = %tunnel_name, binding_count = bindings.len(), "rebuilt tunnel config from current bindings");
    Ok(())
}

// ── Helper: compute MD5 ─────────────────────────────────────────────────

fn compute_md5(input: &str) -> String {
    let mut hasher = Md5::new();
    hasher.update(input.as_bytes());
    let result = hasher.finalize();
    hex::encode(result)
}

// ── Hex encoding (no extra dep) ─────────────────────────────────────────

mod hex {
    pub fn encode(bytes: impl AsRef<[u8]>) -> String {
        bytes
            .as_ref()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }
}

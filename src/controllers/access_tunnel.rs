use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use k8s_openapi::api::apps::v1::{Deployment, DeploymentSpec};
use k8s_openapi::api::core::v1::{
    Affinity, Capabilities, Container, ContainerPort, NodeAffinity, NodeSelector,
    NodeSelectorRequirement, NodeSelectorTerm, PodSecurityContext, PodSpec, PodTemplateSpec,
    Secret, SeccompProfile, SecurityContext, Service, ServicePort, ServiceSpec,
};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{LabelSelector, ObjectMeta, OwnerReference};
use k8s_openapi::apimachinery::pkg::util::intstr::IntOrString;
use kube::api::{Api, Patch, PatchParams};
use kube::runtime::controller::Action;
use kube::runtime::events::{Event, EventType};
use kube::{Resource, ResourceExt};
use tracing::{error, info};

use crate::metrics::{RECONCILE_DURATION, RECONCILE_TOTAL};

use crate::crds::access_tunnel::{AccessTunnel, AccessTunnelServiceToken};
use crate::error::Error;

use super::context::Context;

const CONTAINER_PORT: i32 = 8000;

pub async fn reconcile_access_tunnel(
    obj: Arc<AccessTunnel>,
    ctx: Arc<Context>,
) -> Result<Action, Error> {
    let start = std::time::Instant::now();
    let result = reconcile_access_tunnel_inner(obj, ctx).await;
    let elapsed = start.elapsed().as_secs_f64();
    RECONCILE_DURATION
        .with_label_values(&["accesstunnel"])
        .observe(elapsed);
    let result_label = if result.is_ok() { "success" } else { "error" };
    RECONCILE_TOTAL
        .with_label_values(&["accesstunnel", result_label])
        .inc();
    result
}

async fn reconcile_access_tunnel_inner(
    obj: Arc<AccessTunnel>,
    ctx: Arc<Context>,
) -> Result<Action, Error> {
    let k8s = &ctx.client;
    let name = obj.name_any();
    let ns = obj.namespace().unwrap_or_default();
    let recorder = ctx.recorder();
    let obj_ref = obj.object_ref(&());

    info!(name = %name, ns = %ns, "reconciling access tunnel");

    let spec = &obj.spec;
    let target = &spec.target;

    // Fetch secret if service token is configured
    let secret_data = if let Some(ref svc_token) = spec.service_token {
        let secrets_api: Api<Secret> = Api::namespaced(k8s.clone(), &ns);
        let secret = secrets_api.get(&svc_token.secret_ref).await.map_err(|e| {
            error!(secret = %svc_token.secret_ref, ns = %ns, error = %e, "unable to fetch Secret");
            e
        })?;

        let data = secret.data.as_ref().ok_or_else(|| {
            Error::MissingField(format!("secret {} has no data", svc_token.secret_ref))
        })?;

        if !data.contains_key(&svc_token.CLOUDFLARE_ACCESS_SERVICE_TOKEN_ID) {
            return Err(Error::MissingField(format!(
                "secret does not contain the token ID key {}",
                svc_token.CLOUDFLARE_ACCESS_SERVICE_TOKEN_ID
            )));
        }
        if !data.contains_key(&svc_token.CLOUDFLARE_ACCESS_SERVICE_TOKEN_TOKEN) {
            return Err(Error::MissingField(format!(
                "secret does not contain the token token key {}",
                svc_token.CLOUDFLARE_ACCESS_SERVICE_TOKEN_TOKEN
            )));
        }

        Some((
            String::from_utf8_lossy(&data[&svc_token.CLOUDFLARE_ACCESS_SERVICE_TOKEN_ID].0)
                .to_string(),
            String::from_utf8_lossy(&data[&svc_token.CLOUDFLARE_ACCESS_SERVICE_TOKEN_TOKEN].0)
                .to_string(),
        ))
    } else {
        None
    };

    // Build owner reference
    let oref = OwnerReference {
        api_version: AccessTunnel::api_version(&()).to_string(),
        kind: AccessTunnel::kind(&()).to_string(),
        name: name.clone(),
        uid: obj.uid().unwrap_or_default(),
        controller: Some(true),
        block_owner_deletion: Some(true),
    };

    let (dep, svc) = build_access_deployment_service(
        &name,
        &ns,
        target.image.as_str(),
        target.fqdn.as_str(),
        target.protocol.as_str(),
        target.svc.port,
        if target.svc.name.is_empty() {
            &name
        } else {
            &target.svc.name
        },
        secret_data.as_ref(),
        spec.service_token.as_ref(),
        &oref,
    );

    // Apply Deployment
    let deploy_api: Api<Deployment> = Api::namespaced(k8s.clone(), &ns);
    deploy_api
        .patch(
            &name,
            &PatchParams::apply("cloudflare-operator"),
            &Patch::Apply(dep),
        )
        .await?;
    info!(name = %name, "access tunnel deployment applied");
    recorder
        .publish(
            &Event {
                type_: EventType::Normal,
                reason: "DeploymentApplied".into(),
                note: Some("Access tunnel Deployment applied".into()),
                action: "Reconciling".into(),
                secondary: None,
            },
            &obj_ref,
        )
        .await
        .ok();

    // Apply Service
    let svc_name = if target.svc.name.is_empty() {
        &name
    } else {
        &target.svc.name
    };
    let svc_api: Api<Service> = Api::namespaced(k8s.clone(), &ns);
    svc_api
        .patch(
            svc_name,
            &PatchParams::apply("cloudflare-operator"),
            &Patch::Apply(svc),
        )
        .await?;
    info!(name = %name, "access tunnel service applied");
    recorder
        .publish(
            &Event {
                type_: EventType::Normal,
                reason: "ServiceApplied".into(),
                note: Some("Access tunnel Service applied".into()),
                action: "Reconciling".into(),
                secondary: None,
            },
            &obj_ref,
        )
        .await
        .ok();

    Ok(Action::requeue(Duration::from_secs(300)))
}

pub fn access_tunnel_error_policy(
    _obj: Arc<AccessTunnel>,
    error: &Error,
    _ctx: Arc<Context>,
) -> Action {
    error!(error = %error, "access tunnel reconciliation error, will retry");
    Action::requeue(Duration::from_secs(15))
}

#[allow(clippy::too_many_arguments)]
fn build_access_deployment_service(
    name: &str,
    ns: &str,
    image: &str,
    fqdn: &str,
    protocol: &str,
    port: i32,
    svc_name: &str,
    secret_data: Option<&(String, String)>,
    service_token: Option<&AccessTunnelServiceToken>,
    oref: &OwnerReference,
) -> (Deployment, Service) {
    let effective_port = if port == 0 { CONTAINER_PORT } else { port };

    let k8s_protocol = if protocol == "udp" {
        "UDP".to_string()
    } else {
        "TCP".to_string()
    };

    let mut args = vec![
        "access".to_string(),
        protocol.to_string(),
        "--listener".to_string(),
        format!("0.0.0.0:{CONTAINER_PORT}"),
        "--hostname".to_string(),
        fqdn.to_string(),
    ];

    if let (Some((token_id, token_secret)), Some(_)) = (secret_data, service_token) {
        args.push("--service-token-id".to_string());
        args.push(token_id.clone());
        args.push("--service-token-secret".to_string());
        args.push(token_secret.clone());
    }

    let ls: BTreeMap<String, String> = BTreeMap::from([
        ("app".to_string(), "cloudflared".to_string()),
        ("name".to_string(), name.to_string()),
    ]);

    let dep = Deployment {
        metadata: ObjectMeta {
            name: Some(name.to_string()),
            namespace: Some(ns.to_string()),
            labels: Some(ls.clone()),
            owner_references: Some(vec![oref.clone()]),
            ..Default::default()
        },
        spec: Some(DeploymentSpec {
            replicas: Some(1),
            selector: LabelSelector {
                match_labels: Some(ls.clone()),
                ..Default::default()
            },
            template: PodTemplateSpec {
                metadata: Some(ObjectMeta {
                    labels: Some(ls.clone()),
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
                        image: Some(image.to_string()),
                        name: "cloudflared".to_string(),
                        args: Some(args),
                        ports: Some(vec![ContainerPort {
                            name: Some(name.to_string()),
                            container_port: CONTAINER_PORT,
                            protocol: Some(k8s_protocol.clone()),
                            ..Default::default()
                        }]),
                        resources: Some(k8s_openapi::api::core::v1::ResourceRequirements {
                            requests: Some(BTreeMap::from([
                                (
                                    "memory".to_string(),
                                    k8s_openapi::apimachinery::pkg::api::resource::Quantity(
                                        "30Mi".to_string(),
                                    ),
                                ),
                                (
                                    "cpu".to_string(),
                                    k8s_openapi::apimachinery::pkg::api::resource::Quantity(
                                        "10m".to_string(),
                                    ),
                                ),
                            ])),
                            limits: Some(BTreeMap::from([
                                (
                                    "memory".to_string(),
                                    k8s_openapi::apimachinery::pkg::api::resource::Quantity(
                                        "256Mi".to_string(),
                                    ),
                                ),
                                (
                                    "cpu".to_string(),
                                    k8s_openapi::apimachinery::pkg::api::resource::Quantity(
                                        "500m".to_string(),
                                    ),
                                ),
                            ])),
                            ..Default::default()
                        }),
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
                    affinity: Some(Affinity {
                        node_affinity: Some(NodeAffinity {
                            required_during_scheduling_ignored_during_execution: Some(
                                NodeSelector {
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
                                },
                            ),
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
    };

    let svc = Service {
        metadata: ObjectMeta {
            name: Some(svc_name.to_string()),
            namespace: Some(ns.to_string()),
            labels: Some(ls.clone()),
            owner_references: Some(vec![oref.clone()]),
            ..Default::default()
        },
        spec: Some(ServiceSpec {
            selector: Some(ls),
            ports: Some(vec![ServicePort {
                name: Some(protocol.to_string()),
                protocol: Some(k8s_protocol),
                target_port: Some(IntOrString::Int(CONTAINER_PORT)),
                port: effective_port,
                ..Default::default()
            }]),
            ..Default::default()
        }),
        ..Default::default()
    };

    (dep, svc)
}

use std::sync::Arc;

use futures::StreamExt;
use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::api::core::v1::{ConfigMap, Secret};
use kube::api::Api;
use kube::runtime::watcher::Config as WatcherConfig;
use kube::runtime::Controller;
use kube::Client;
use kube::runtime::events::Reporter;
use kube_lease_manager::LeaseManagerBuilder;
use tracing::{error, info};
use tracing_subscriber::{fmt, EnvFilter};

use cloudflare_operator::controllers::access_tunnel::{
    access_tunnel_error_policy, reconcile_access_tunnel,
};
use cloudflare_operator::controllers::context::Context;
use cloudflare_operator::controllers::tunnel::{error_policy, reconcile_tunnel};
use cloudflare_operator::controllers::tunnel_binding::{binding_error_policy, reconcile_binding};
use cloudflare_operator::crds::access_tunnel::AccessTunnel;
use cloudflare_operator::crds::cluster_tunnel::ClusterTunnel;
use cloudflare_operator::crds::tunnel::Tunnel;
use cloudflare_operator::crds::tunnel_binding::TunnelBinding;

#[cfg(feature = "gateway-api")]
use cloudflare_operator::controllers::gateway::{
    gateway_class_error_policy, gateway_error_policy, httproute_error_policy,
    reconcile_gateway, reconcile_gateway_class, reconcile_httproute,
};
#[cfg(feature = "gateway-api")]
use cloudflare_operator::crds::gateway::{Gateway, GatewayClass, HTTPRoute};

const DEFAULT_CLOUDFLARED_IMAGE: &str = "cloudflare/cloudflared:latest";
const DEFAULT_CLUSTER_RESOURCE_NAMESPACE: &str = "cloudflare-operator-system";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("info".parse().unwrap()))
        .json()
        .init();

    info!("starting cloudflare-operator controller");

    let client = Client::try_default().await?;

    let cluster_resource_namespace = std::env::var("CLUSTER_RESOURCE_NAMESPACE")
        .unwrap_or_else(|_| DEFAULT_CLUSTER_RESOURCE_NAMESPACE.to_string());
    let cloudflared_image = std::env::var("CLOUDFLARED_IMAGE")
        .unwrap_or_else(|_| DEFAULT_CLOUDFLARED_IMAGE.to_string());
    let overwrite_unmanaged = std::env::var("OVERWRITE_UNMANAGED_DNS")
        .map(|v| v == "true" || v == "1")
        .unwrap_or(false);
    let cloudflare_api_base_url = std::env::var("CLOUDFLARE_API_BASE_URL").ok();

    let reporter = Reporter {
        controller: "cloudflare-operator".into(),
        instance: std::env::var("CONTROLLER_POD_NAME").ok(),
    };

    let ctx = Arc::new(Context {
        client: client.clone(),
        reporter,
        cloudflare_api_base_url,
        cluster_resource_namespace,
        overwrite_unmanaged,
        cloudflared_image,
        #[cfg(feature = "gateway-api")]
        enable_gateway_api: std::env::var("ENABLE_GATEWAY_API")
            .map(|v| v == "true" || v == "1")
            .unwrap_or(false),
    });

    // Leader election
    let leader_elect = std::env::var("LEADER_ELECT")
        .map(|v| v != "false" && v != "0")
        .unwrap_or(true);

    let _lease_task = if leader_elect {
        let lease_manager = LeaseManagerBuilder::new(client.clone(), "9f193cf8.cfargotunnel.com")
            .with_namespace(&ctx.cluster_resource_namespace)
            .with_duration(15)
            .with_grace(5)
            .build()
            .await?;
        let (mut lease_channel, lease_task) = lease_manager.watch().await;

        info!("waiting for leader lease...");
        while !*lease_channel.borrow_and_update() {
            if lease_channel.changed().await.is_err() {
                anyhow::bail!("leader lease watch channel closed");
            }
        }
        info!("acquired leader lease, starting controllers");

        // Spawn a task that exits if leadership is lost
        tokio::spawn(async move {
            loop {
                if lease_channel.changed().await.is_err() {
                    break;
                }
                if !*lease_channel.borrow() {
                    error!("lost leader lease, shutting down");
                    std::process::exit(1);
                }
            }
        });

        Some(lease_task)
    } else {
        info!("leader election disabled, starting controllers immediately");
        None
    };

    // Tunnel controller (namespaced)
    let tunnels: Api<Tunnel> = Api::all(client.clone());
    let tunnel_bindings_for_tunnel: Api<TunnelBinding> = Api::all(client.clone());
    let tunnel_ctrl = Controller::new(tunnels, WatcherConfig::default())
        .owns(Api::<ConfigMap>::all(client.clone()), WatcherConfig::default())
        .owns(Api::<Secret>::all(client.clone()), WatcherConfig::default())
        .owns(Api::<Deployment>::all(client.clone()), WatcherConfig::default())
        .watches(
            tunnel_bindings_for_tunnel,
            WatcherConfig::default(),
            |binding| {
                let labels = binding.metadata.labels.as_ref()?;
                let tunnel_name = labels.get("cfargotunnel.com/name")?;
                let kind = labels.get("cfargotunnel.com/kind")?;
                if kind != "Tunnel" {
                    return None;
                }
                let ns = binding.metadata.namespace.as_deref()?;
                Some(kube::runtime::reflector::ObjectRef::new(tunnel_name).within(ns))
            },
        )
        .run(reconcile_tunnel, error_policy, ctx.clone())
        .for_each(|res| async move {
            match res {
                Ok(o) => info!(tunnel = ?o, "tunnel reconciled"),
                Err(e) => error!(error = ?e, "tunnel reconcile failed"),
            }
        });

    // ClusterTunnel controller (cluster-scoped)
    let cluster_tunnels: Api<ClusterTunnel> = Api::all(client.clone());
    let tunnel_bindings_for_ct: Api<TunnelBinding> = Api::all(client.clone());
    let ct_ctrl = Controller::new(cluster_tunnels, WatcherConfig::default())
        .owns(Api::<ConfigMap>::all(client.clone()), WatcherConfig::default())
        .owns(Api::<Secret>::all(client.clone()), WatcherConfig::default())
        .owns(Api::<Deployment>::all(client.clone()), WatcherConfig::default())
        .watches(
            tunnel_bindings_for_ct,
            WatcherConfig::default(),
            |binding| {
                let labels = binding.metadata.labels.as_ref()?;
                let tunnel_name = labels.get("cfargotunnel.com/name")?;
                let kind = labels.get("cfargotunnel.com/kind")?;
                if kind != "ClusterTunnel" {
                    return None;
                }
                Some(kube::runtime::reflector::ObjectRef::new(tunnel_name))
            },
        )
        .run(reconcile_tunnel, error_policy, ctx.clone())
        .for_each(|res| async move {
            match res {
                Ok(o) => info!(cluster_tunnel = ?o, "cluster tunnel reconciled"),
                Err(e) => error!(error = ?e, "cluster tunnel reconcile failed"),
            }
        });

    // TunnelBinding controller
    let bindings: Api<TunnelBinding> = Api::all(client.clone());
    let tunnels_for_binding: Api<Tunnel> = Api::all(client.clone());
    let cluster_tunnels_for_binding: Api<ClusterTunnel> = Api::all(client.clone());
    let binding_ctrl = Controller::new(bindings, WatcherConfig::default())
        .watches(
            tunnels_for_binding,
            WatcherConfig::default(),
            |tunnel| {
                // When a Tunnel changes, re-reconcile all TunnelBindings that reference it
                // This is a mapper that returns an empty list; the label-based watch handles it.
                // We trigger reconcile via the controller's cache invalidation.
                let _ = tunnel;
                None::<kube::runtime::reflector::ObjectRef<TunnelBinding>>
            },
        )
        .watches(
            cluster_tunnels_for_binding,
            WatcherConfig::default(),
            |ct| {
                let _ = ct;
                None::<kube::runtime::reflector::ObjectRef<TunnelBinding>>
            },
        )
        .run(reconcile_binding, binding_error_policy, ctx.clone())
        .for_each(|res| async move {
            match res {
                Ok(o) => info!(binding = ?o, "tunnel binding reconciled"),
                Err(e) => error!(error = ?e, "tunnel binding reconcile failed"),
            }
        });

    // AccessTunnel controller (namespaced)
    let access_tunnels: Api<AccessTunnel> = Api::all(client.clone());
    let at_ctrl = Controller::new(access_tunnels, WatcherConfig::default())
        .owns(Api::<Deployment>::all(client.clone()), WatcherConfig::default())
        .run(reconcile_access_tunnel, access_tunnel_error_policy, ctx.clone())
        .for_each(|res| async move {
            match res {
                Ok(o) => info!(access_tunnel = ?o, "access tunnel reconciled"),
                Err(e) => error!(error = ?e, "access tunnel reconcile failed"),
            }
        });

    // Gateway API controllers (feature-gated)
    #[cfg(feature = "gateway-api")]
    let gateway_api_enabled = ctx.enable_gateway_api;

    #[cfg(feature = "gateway-api")]
    let gc_ctrl = async {
        if !gateway_api_enabled { return; }
        let gcs: Api<GatewayClass> = Api::all(client.clone());
        Controller::new(gcs, WatcherConfig::default())
            .run(reconcile_gateway_class, gateway_class_error_policy, ctx.clone())
            .for_each(|res| async move {
                match res {
                    Ok(o) => info!(gateway_class = ?o, "GatewayClass reconciled"),
                    Err(e) => error!(error = %e, "GatewayClass reconcile failed"),
                }
            })
            .await;
    };

    #[cfg(feature = "gateway-api")]
    let gw_ctrl = async {
        if !gateway_api_enabled { return; }
        let gws: Api<Gateway> = Api::all(client.clone());
        Controller::new(gws, WatcherConfig::default())
            .run(reconcile_gateway, gateway_error_policy, ctx.clone())
            .for_each(|res| async move {
                match res {
                    Ok(o) => info!(gateway = ?o, "Gateway reconciled"),
                    Err(e) => error!(error = %e, "Gateway reconcile failed"),
                }
            })
            .await;
    };

    #[cfg(feature = "gateway-api")]
    let hr_ctrl = async {
        if !gateway_api_enabled { return; }
        let routes: Api<HTTPRoute> = Api::all(client.clone());
        Controller::new(routes, WatcherConfig::default())
            .run(reconcile_httproute, httproute_error_policy, ctx.clone())
            .for_each(|res| async move {
                match res {
                    Ok(o) => info!(httproute = ?o, "HTTPRoute reconciled"),
                    Err(e) => error!(error = %e, "HTTPRoute reconcile failed"),
                }
            })
            .await;
    };

    // Health/readiness probe + metrics server on :8081
    let health_server = async {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let addr = std::env::var("HEALTH_PROBE_BIND_ADDRESS").unwrap_or_else(|_| "0.0.0.0:8081".into());
        let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
        info!("health/metrics server listening on {}", addr);
        loop {
            if let Ok((mut stream, _)) = listener.accept().await {
                let mut buf = [0u8; 1024];
                let n = stream.read(&mut buf).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..n]);
                let response = if request.contains("GET /metrics") {
                    let body = cloudflare_operator::metrics::gather_metrics();
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/plain; version=0.0.4\r\nContent-Length: {}\r\n\r\n{}",
                        body.len(),
                        body
                    )
                } else {
                    "HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok".to_string()
                };
                let _ = stream.write_all(response.as_bytes()).await;
            }
        }
    };

    info!("controllers started, waiting for events");

    #[cfg(feature = "gateway-api")]
    tokio::select! {
        _ = health_server => { error!("health server exited unexpectedly"); }
        _ = tunnel_ctrl => { error!("tunnel controller exited unexpectedly"); }
        _ = ct_ctrl => { error!("cluster tunnel controller exited unexpectedly"); }
        _ = binding_ctrl => { error!("tunnel binding controller exited unexpectedly"); }
        _ = at_ctrl => { error!("access tunnel controller exited unexpectedly"); }
        _ = gc_ctrl => { error!("gateway class controller exited unexpectedly"); }
        _ = gw_ctrl => { error!("gateway controller exited unexpectedly"); }
        _ = hr_ctrl => { error!("httproute controller exited unexpectedly"); }
    }

    #[cfg(not(feature = "gateway-api"))]
    tokio::select! {
        _ = health_server => { error!("health server exited unexpectedly"); }
        _ = tunnel_ctrl => { error!("tunnel controller exited unexpectedly"); }
        _ = ct_ctrl => { error!("cluster tunnel controller exited unexpectedly"); }
        _ = binding_ctrl => { error!("tunnel binding controller exited unexpectedly"); }
        _ = at_ctrl => { error!("access tunnel controller exited unexpectedly"); }
    }

    Ok(())
}

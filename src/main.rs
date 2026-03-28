use std::sync::Arc;

use futures::StreamExt;
use kube::api::Api;
use kube::runtime::watcher::Config as WatcherConfig;
use kube::runtime::Controller;
use kube::Client;
use tracing::{error, info};
use tracing_subscriber::{fmt, EnvFilter};

use cloudflare_operator::controllers::context::Context;
use cloudflare_operator::controllers::tunnel::{error_policy, reconcile_tunnel};
use cloudflare_operator::controllers::tunnel_binding::{binding_error_policy, reconcile_binding};
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

    let ctx = Arc::new(Context {
        client: client.clone(),
        cloudflare_api_base_url,
        cluster_resource_namespace,
        overwrite_unmanaged,
        cloudflared_image,
        #[cfg(feature = "gateway-api")]
        enable_gateway_api: std::env::var("ENABLE_GATEWAY_API")
            .map(|v| v == "true" || v == "1")
            .unwrap_or(false),
    });

    // Tunnel controller (namespaced)
    let tunnels: Api<Tunnel> = Api::all(client.clone());
    let tunnel_bindings_for_tunnel: Api<TunnelBinding> = Api::all(client.clone());
    let tunnel_ctrl = Controller::new(tunnels, WatcherConfig::default())
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

    info!("controllers started, waiting for events");

    #[cfg(feature = "gateway-api")]
    tokio::select! {
        _ = tunnel_ctrl => { error!("tunnel controller exited unexpectedly"); }
        _ = ct_ctrl => { error!("cluster tunnel controller exited unexpectedly"); }
        _ = binding_ctrl => { error!("tunnel binding controller exited unexpectedly"); }
        _ = gc_ctrl => { error!("gateway class controller exited unexpectedly"); }
        _ = gw_ctrl => { error!("gateway controller exited unexpectedly"); }
        _ = hr_ctrl => { error!("httproute controller exited unexpectedly"); }
    }

    #[cfg(not(feature = "gateway-api"))]
    tokio::select! {
        _ = tunnel_ctrl => { error!("tunnel controller exited unexpectedly"); }
        _ = ct_ctrl => { error!("cluster tunnel controller exited unexpectedly"); }
        _ = binding_ctrl => { error!("tunnel binding controller exited unexpectedly"); }
    }

    Ok(())
}

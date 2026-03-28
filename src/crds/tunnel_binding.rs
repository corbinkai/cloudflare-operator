use std::borrow::Cow;

use k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::{
    CustomResourceDefinition, CustomResourceDefinitionNames, CustomResourceDefinitionSpec,
    CustomResourceDefinitionVersion, CustomResourceSubresourceStatus, CustomResourceSubresources,
    CustomResourceValidation, JSONSchemaProps,
};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use kube::Resource;
use serde::{Deserialize, Serialize};

use super::types::{ServiceInfo, TunnelBindingSubject, TunnelRef};

/// TunnelBinding is the Schema for the tunnelbindings API.
/// This CRD has subjects and tunnelRef at the top level (not under spec),
/// so we implement the Resource trait manually instead of using the CustomResource derive.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TunnelBinding {
    /// Standard Kubernetes API metadata
    pub metadata: ObjectMeta,

    /// Subjects defines the services this binding connects to the tunnel
    pub subjects: Vec<TunnelBindingSubject>,

    /// TunnelRef defines the Tunnel this binding connects to
    pub tunnel_ref: TunnelRef,

    /// Status defines the observed state of TunnelBinding
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<TunnelBindingStatus>,
}

/// TunnelBindingStatus defines the observed state of TunnelBinding.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct TunnelBindingStatus {
    /// Comma-separated list of hostnames for display
    pub hostnames: String,

    /// List of services with hostname and target
    pub services: Vec<ServiceInfo>,
}

/// TunnelBindingList contains a list of TunnelBinding.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct TunnelBindingList {
    pub metadata: k8s_openapi::apimachinery::pkg::apis::meta::v1::ListMeta,
    pub items: Vec<TunnelBinding>,
}

impl Resource for TunnelBinding {
    type DynamicType = ();
    type Scope = k8s_openapi::NamespaceResourceScope;

    fn kind(_dt: &()) -> Cow<'_, str> {
        Cow::Borrowed("TunnelBinding")
    }

    fn group(_dt: &()) -> Cow<'_, str> {
        Cow::Borrowed("networking.cfargotunnel.com")
    }

    fn version(_dt: &()) -> Cow<'_, str> {
        Cow::Borrowed("v1alpha1")
    }

    fn plural(_dt: &()) -> Cow<'_, str> {
        Cow::Borrowed("tunnelbindings")
    }

    fn meta(&self) -> &ObjectMeta {
        &self.metadata
    }

    fn meta_mut(&mut self) -> &mut ObjectMeta {
        &mut self.metadata
    }

    fn api_version(_dt: &()) -> Cow<'_, str> {
        Cow::Borrowed("networking.cfargotunnel.com/v1alpha1")
    }

    fn url_path(dt: &(), namespace: Option<&str>) -> String {
        let prefix = "/apis/networking.cfargotunnel.com/v1alpha1";
        match namespace {
            Some(ns) => format!("{prefix}/namespaces/{ns}/{}", Self::plural(dt)),
            None => format!("{prefix}/{}", Self::plural(dt)),
        }
    }
}

impl TunnelBinding {
    /// Build the CustomResourceDefinition object for TunnelBinding.
    /// Since we don't use the derive macro, we construct this manually.
    pub fn crd() -> CustomResourceDefinition {
        // Build the schema properties manually since we can't derive JsonSchema on ObjectMeta
        let mut properties = std::collections::BTreeMap::new();
        let mut required = Vec::new();

        // subjects: array of TunnelBindingSubject
        properties.insert(
            "subjects".to_string(),
            JSONSchemaProps {
                type_: Some("array".to_string()),
                items: Some(k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::JSONSchemaPropsOrArray::Schema(
                    Box::new(JSONSchemaProps {
                        type_: Some("object".to_string()),
                        ..Default::default()
                    }),
                )),
                ..Default::default()
            },
        );
        required.push("subjects".to_string());

        // tunnelRef: object
        properties.insert(
            "tunnelRef".to_string(),
            JSONSchemaProps {
                type_: Some("object".to_string()),
                ..Default::default()
            },
        );
        required.push("tunnelRef".to_string());

        // status: object
        properties.insert(
            "status".to_string(),
            JSONSchemaProps {
                type_: Some("object".to_string()),
                ..Default::default()
            },
        );

        let openapi_schema = JSONSchemaProps {
            type_: Some("object".to_string()),
            properties: Some(properties),
            required: Some(required),
            ..Default::default()
        };

        CustomResourceDefinition {
            metadata: ObjectMeta {
                name: Some("tunnelbindings.networking.cfargotunnel.com".to_string()),
                ..Default::default()
            },
            spec: CustomResourceDefinitionSpec {
                group: "networking.cfargotunnel.com".to_string(),
                names: CustomResourceDefinitionNames {
                    kind: "TunnelBinding".to_string(),
                    plural: "tunnelbindings".to_string(),
                    singular: Some("tunnelbinding".to_string()),
                    short_names: None,
                    ..Default::default()
                },
                scope: "Namespaced".to_string(),
                versions: vec![CustomResourceDefinitionVersion {
                    name: "v1alpha1".to_string(),
                    served: true,
                    storage: true,
                    schema: Some(CustomResourceValidation {
                        open_api_v3_schema: Some(openapi_schema),
                    }),
                    subresources: Some(CustomResourceSubresources {
                        status: Some(CustomResourceSubresourceStatus(serde_json::Value::Object(
                            Default::default(),
                        ))),
                        ..Default::default()
                    }),
                    additional_printer_columns: Some(vec![
                        k8s_openapi::apiextensions_apiserver::pkg::apis::apiextensions::v1::CustomResourceColumnDefinition {
                            name: "FQDNs".to_string(),
                            type_: "string".to_string(),
                            json_path: ".status.hostnames".to_string(),
                            ..Default::default()
                        },
                    ]),
                    ..Default::default()
                }],
                ..Default::default()
            },
            status: None,
        }
    }
}

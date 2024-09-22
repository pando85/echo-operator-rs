use crate::controller::Context;
use crate::crd::echo::Echo;
use crate::echo::finalizer;
use crate::error::{Error, Result};
use crate::telemetry;

use k8s_openapi::api::apps::v1::{Deployment, DeploymentSpec};
use k8s_openapi::api::core::v1::{Container, ContainerPort, PodSpec, PodTemplateSpec};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::LabelSelector;
use kube::api::{Api, DeleteParams, ObjectMeta, PostParams};
use kube::client::Client;
use kube::runtime::controller::Action;
use kube::{Resource, ResourceExt};
use std::collections::BTreeMap;
use std::sync::Arc;
use tokio::time::Duration;
use tracing::{field, info, instrument, Span};

/// Creates a new deployment of `n` pods with the `inanimate/echo-server:latest` docker image inside,
/// where `n` is the number of `replicas` given.
pub async fn deploy(
    client: Client,
    name: &str,
    replicas: i32,
    namespace: &str,
) -> Result<Deployment, Error> {
    let mut labels: BTreeMap<String, String> = BTreeMap::new();
    labels.insert("app".to_owned(), name.to_owned());

    let deployment: Deployment = Deployment {
        metadata: ObjectMeta {
            name: Some(name.to_owned()),
            namespace: Some(namespace.to_owned()),
            labels: Some(labels.clone()),
            ..ObjectMeta::default()
        },
        spec: Some(DeploymentSpec {
            replicas: Some(replicas),
            selector: LabelSelector {
                match_expressions: None,
                match_labels: Some(labels.clone()),
            },
            template: PodTemplateSpec {
                spec: Some(PodSpec {
                    containers: vec![Container {
                        name: name.to_owned(),
                        image: Some("inanimate/echo-server:latest".to_owned()),
                        ports: Some(vec![ContainerPort {
                            container_port: 8080,
                            ..ContainerPort::default()
                        }]),
                        ..Container::default()
                    }],
                    ..PodSpec::default()
                }),
                metadata: Some(ObjectMeta {
                    labels: Some(labels),
                    ..ObjectMeta::default()
                }),
            },
            ..DeploymentSpec::default()
        }),
        ..Deployment::default()
    };

    let deployment_api: Api<Deployment> = Api::namespaced(client, namespace);
    deployment_api
        .create(&PostParams::default(), &deployment)
        .await
        .map_err(Error::KubeError)
}

/// Deletes an existing deployment.
pub async fn delete(client: Client, name: &str, namespace: &str) -> Result<(), Error> {
    let api: Api<Deployment> = Api::namespaced(client, namespace);
    api.delete(name, &DeleteParams::default())
        .await
        .map_err(Error::KubeError)?;
    Ok(())
}

/// Action to be taken upon an `Echo` resource during reconciliation
enum EchoAction {
    /// Create the subresources, this includes spawning `n` pods with Echo service
    Create,
    /// Delete all subresources created in the `Create` phase
    Delete,
    /// This `Echo` resource is in desired state and requires no actions to be taken
    NoOp,
}

/// Determines the appropriate action to take for the given `Echo` resource.
///
/// This function checks the `Echo` resource's metadata to decide whether the
/// resource should be deleted, created, or no operation is needed.
/// - If the `deletion_timestamp` is set, the action is `Delete`.
/// - If there are no finalizers or the finalizers list is empty, the action is `Create`.
/// - Otherwise, the action is `NoOp`.
fn determine_action(echo: &Echo) -> EchoAction {
    match echo.meta().deletion_timestamp {
        Some(_) => EchoAction::Delete,
        None => match echo.meta().finalizers.as_ref() {
            Some(finalizers) if finalizers.is_empty() => EchoAction::Create,
            None => EchoAction::Create,
            _ => EchoAction::NoOp,
        },
    }
}

#[instrument(skip(ctx, echo), fields(trace_id))]
pub async fn reconcile(echo: Arc<Echo>, ctx: Arc<Context>) -> Result<Action, Error> {
    let trace_id = telemetry::get_trace_id();
    Span::current().record("trace_id", field::display(&trace_id));
    let _timer = ctx.metrics.reconcile.count_and_measure(&trace_id);

    let namespace = echo.namespace().ok_or_else(|| {
        Error::UserInputError(
            "Expected Echo resource to be namespaced. Can't deploy to an unknown namespace."
                .to_owned(),
        )
    })?;
    let name = echo.name_any();
    info!("Reconciling Echo \"{name}\" in {namespace}");

    let client: Client = ctx.client.clone();
    match determine_action(&echo) {
        EchoAction::Create => {
            // Adds a finalizer to the `Echo` resource to prevent it from being deleted before
            finalizer::add(client.clone(), &name, &namespace)
                .await
                .map_err(Error::KubeError)?;
            deploy(client, &name, echo.spec.replicas, &namespace).await?;
            Ok(Action::requeue(Duration::from_secs(10)))
        }
        EchoAction::Delete => {
            delete(client.clone(), &name, &namespace).await?;

            // Once the deployment is successfully removed, remove the finalizer to make it possible
            // for Kubernetes to delete the `Echo` resource.
            finalizer::delete(client, &name, &namespace)
                .await
                .map_err(Error::KubeError)?;
            Ok(Action::await_change())
        }
        // The resource is already in desired state, do nothing and re-check after 10 seconds
        EchoAction::NoOp => Ok(Action::requeue(Duration::from_secs(10))),
    }
}

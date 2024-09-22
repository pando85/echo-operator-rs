use crate::controller::Context;
use crate::crd::echo::Echo;
use crate::error::{Error, Result};
use crate::telemetry;

use k8s_openapi::api::apps::v1::{Deployment, DeploymentSpec};
use k8s_openapi::api::core::v1::{Container, ContainerPort, PodSpec, PodTemplateSpec};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::LabelSelector;
use kube::api::{Api, DeleteParams, ObjectMeta, Patch, PatchParams, PostParams};
use kube::client::Client;
use kube::runtime::controller::Action;
use kube::runtime::finalizer::{finalizer, Event};
use kube::ResourceExt;
use std::collections::BTreeMap;
use std::sync::Arc;
use tokio::time::Duration;
use tracing::{field, info, instrument, Span};

pub static ECHO_FINALIZER: &str = "echoes.example.com";

fn build_deployment(name: &str, namespace: &str, replicas: i32) -> Deployment {
    let mut labels: BTreeMap<String, String> = BTreeMap::new();
    labels.insert("app".to_owned(), name.to_owned());
    Deployment {
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
    }
}

pub async fn create(
    client: Client,
    name: &str,
    replicas: i32,
    namespace: &str,
) -> Result<Deployment, Error> {
    let deployment = build_deployment(name, namespace, replicas);

    let deployment_api: Api<Deployment> = Api::namespaced(client, namespace);
    deployment_api
        .create(&PostParams::default(), &deployment)
        .await
        .map_err(Error::KubeError)
}

async fn patch(
    client: Client,
    name: &str,
    replicas: i32,
    namespace: &str,
) -> Result<Deployment, Error> {
    let deployment_api: Api<Deployment> = Api::namespaced(client, namespace);
    let deployment = build_deployment(name, namespace, replicas);

    deployment_api
        .patch(name, &PatchParams::default(), &Patch::Merge(&deployment))
        .await
        .map_err(Error::KubeError)
}

pub async fn delete(client: Client, name: &str, namespace: &str) -> Result<(), Error> {
    let api: Api<Deployment> = Api::namespaced(client, namespace);
    api.delete(name, &DeleteParams::default())
        .await
        .map_err(Error::KubeError)?;
    Ok(())
}

impl Echo {
    fn get_namespace(&self) -> Result<String> {
        self.namespace().ok_or_else(|| {
            Error::UserInputError(
                "Expected Echo resource to be namespaced. Can't deploy to an unknown namespace."
                    .to_owned(),
            )
        })
    }
    async fn reconcile(&self, ctx: Arc<Context>) -> Result<()> {
        let deployment_api: Api<Deployment> =
            Api::namespaced(ctx.client.clone(), &self.get_namespace()?);
        let current_deployment = match deployment_api.get(&self.name_any()).await {
            Ok(deployment) => deployment,
            Err(_) => {
                create(
                    ctx.client.clone(),
                    &self.name_any(),
                    self.spec.replicas,
                    &self.get_namespace()?,
                )
                .await?;
                return Ok(());
            }
        };

        match current_deployment.spec {
            None => {
                create(
                    ctx.client.clone(),
                    &self.name_any(),
                    self.spec.replicas,
                    &self.get_namespace()?,
                )
                .await?;
                Ok(())
            }
            Some(spec) => match spec.replicas {
                Some(x) if x == self.spec.replicas => Ok(()),
                _ => {
                    patch(
                        ctx.client.clone(),
                        &self.name_any(),
                        self.spec.replicas,
                        &self.get_namespace()?,
                    )
                    .await?;
                    Ok(())
                }
            },
        }
    }

    async fn cleanup(&self, ctx: Arc<Context>) -> Result<Action> {
        delete(ctx.client.clone(), &self.name_any(), &self.get_namespace()?).await?;
        Ok(Action::await_change())
    }
}

#[instrument(skip(ctx, echo), fields(trace_id))]
pub async fn reconcile(echo: Arc<Echo>, ctx: Arc<Context>) -> Result<Action, Error> {
    let trace_id = telemetry::get_trace_id();
    Span::current().record("trace_id", field::display(&trace_id));
    let _timer = ctx.metrics.reconcile.count_and_measure(&trace_id);

    let name = echo.name_any();
    let namespace = echo.get_namespace()?;
    info!("Reconciling Echo \"{name}\" in {namespace}");

    let echoes = Api::<Echo>::all(ctx.client.clone());

    finalizer(&echoes, ECHO_FINALIZER, echo, |event| async {
        match event {
            Event::Apply(echo) => {
                echo.reconcile(ctx.clone()).await?;
                Ok(Action::requeue(Duration::from_secs(5 * 60)))
            }
            Event::Cleanup(echo) => {
                echo.cleanup(ctx.clone()).await?;
                Ok(Action::requeue(Duration::from_secs(5 * 60)))
            }
        }
    })
    .await
    .map_err(|e| Error::FinalizerError(Box::new(e)))
}

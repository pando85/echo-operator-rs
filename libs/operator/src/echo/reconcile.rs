use crate::controller::Context;
use crate::crd::echo::{Echo, EchoStatus};
use crate::error::{Error, Result};
use crate::telemetry;

use std::collections::BTreeMap;
use std::sync::Arc;

use k8s_openapi::api::apps::v1::{Deployment, DeploymentSpec};
use k8s_openapi::api::core::v1::{Container, ContainerPort, PodSpec, PodTemplateSpec};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::LabelSelector;
use kube::api::{Api, ObjectMeta, Patch, PatchParams, Resource};
use kube::client::Client;
use kube::runtime::controller::Action;
use kube::runtime::reflector::ObjectRef;
use kube::ResourceExt;
use serde_json::json;
use tokio::time::Duration;
use tracing::{debug, field, info, instrument, Span};

pub static ECHO_FINALIZER: &str = "echoes.example.com";

async fn patch(client: Client, echo: &Echo) -> Result<Deployment, Error> {
    let deployment_api = Api::<Deployment>::namespaced(client, &echo.get_namespace());
    let owner_references = echo.controller_owner_ref(&()).map(|oref| vec![oref]);

    let mut labels: BTreeMap<String, String> = BTreeMap::new();
    labels.extend(
        echo.labels()
            .iter()
            .map(|(k, v)| (k.to_owned(), v.to_owned())),
    );
    labels.insert("app".to_owned(), echo.name_any());
    labels.insert("app.kubernetes.io/name".to_owned(), "echo".to_owned());
    labels.insert(
        "app.kubernetes.io/managed-by".to_owned(),
        "kaniop".to_owned(),
    );
    let deployment = Deployment {
        metadata: ObjectMeta {
            name: Some(echo.name_any()),
            namespace: Some(echo.get_namespace()),
            labels: Some(labels.clone()),
            owner_references,
            ..ObjectMeta::default()
        },
        spec: Some(DeploymentSpec {
            replicas: Some(echo.spec.replicas),
            selector: LabelSelector {
                match_expressions: None,
                match_labels: Some(labels.clone()),
            },
            template: PodTemplateSpec {
                spec: Some(PodSpec {
                    containers: vec![Container {
                        name: echo.name_any(),
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

    deployment_api
        .patch(
            &echo.name_any(),
            &PatchParams::apply("echoes.example.com").force(),
            &Patch::Apply(&deployment),
        )
        .await
        .map_err(Error::KubeError)
}

impl Echo {
    fn get_namespace(&self) -> String {
        // safe unwrap: Echo is namespaced scoped
        self.namespace().unwrap()
    }
}

pub async fn reconcile_echo_status(echo: &Echo, ctx: Arc<Context<Deployment>>) -> Result<()> {
    let namespace = &echo.get_namespace();
    let deployment_ref = ObjectRef::<Deployment>::new_with(&echo.name_any(), ()).within(namespace);
    debug!("getting deployment: {}/{}", namespace, &echo.name_any());
    let deployment = match ctx.store.get(&deployment_ref) {
        Some(deployment) => Ok(deployment),
        None => Err(Error::MissingObject("deployment")),
    }?;
    let owner = match deployment
        .metadata
        .owner_references
        .as_ref()
        .and_then(|refs| refs.iter().find(|r| r.controller == Some(true)))
    {
        Some(owner) => Ok(owner),
        None => Err(Error::MissingObjectKey("ownerReferences")),
    }?;

    let deployment_status = match deployment.status.as_ref() {
        Some(status) => Ok(status),
        None => Err(Error::MissingObjectKey("status")),
    }?;
    let new_status = EchoStatus {
        available_replicas: deployment_status.available_replicas,
        observed_generation: deployment.metadata.generation,
        ready_replicas: deployment_status.ready_replicas,
        replicas: deployment_status.replicas,
        updated_replicas: deployment_status.updated_replicas,
    };
    let new_status_patch = Patch::Apply(json!({
        "apiVersion": "example.com/v1",
        "kind": "Echo",
        "status": new_status
    }));
    debug!("updating Echo status for: {}/{}", namespace, owner.name);
    let patch = PatchParams::apply("echoes.example.com").force();
    let echo_api = Api::<Echo>::namespaced(ctx.client.clone(), namespace);
    let _o = echo_api
        .patch_status(&owner.name, &patch, &new_status_patch)
        .await
        .map_err(Error::KubeError)?;
    Ok(())
}

#[instrument(skip(ctx, echo), fields(trace_id))]
pub async fn reconcile_echo(echo: Arc<Echo>, ctx: Arc<Context<Deployment>>) -> Result<Action> {
    let trace_id = telemetry::get_trace_id();
    Span::current().record("trace_id", field::display(&trace_id));
    let _timer = ctx.metrics.reconcile.count_and_measure(&trace_id);

    let name = echo.name_any();
    let namespace = echo.get_namespace();
    info!("reconciling Echo: {namespace}/{name}");

    let _ignored_errors = reconcile_echo_status(&echo, ctx.clone()).await;
    patch(ctx.client.clone(), &echo).await?;
    Ok(Action::requeue(Duration::from_secs(5 * 60)))
}

#[cfg(test)]
mod test {
    use super::reconcile_echo;

    use crate::controller::Context;
    use crate::crd::echo::Echo;
    use crate::echo::test::{timeout_after_1s, Scenario};

    use std::sync::Arc;

    #[tokio::test]
    async fn finalized_echo_create() {
        let (testctx, fakeserver) = Context::test();
        let echo = Echo::test().finalized();
        let mocksrv = fakeserver.run(Scenario::EchoPatch(echo.clone()));
        reconcile_echo(Arc::new(echo), testctx)
            .await
            .expect("reconciler");
        timeout_after_1s(mocksrv).await;
    }

    #[tokio::test]
    async fn finalized_echo_causes_status_patch() {
        let (testctx, fakeserver) = Context::test();
        let echo = Echo::test_with_status().finalized();
        let mocksrv = fakeserver.run(Scenario::EchoPatch(echo.clone()));
        reconcile_echo(Arc::new(echo), testctx)
            .await
            .expect("reconciler");
        timeout_after_1s(mocksrv).await;
    }

    #[tokio::test]
    async fn finalized_echo_with_replicas_causes_patch() {
        let (testctx, fakeserver) = Context::test();
        let echo = Echo::test_with_status().finalized().change_replicas(3);
        let scenario = Scenario::EchoPatch(echo.clone());
        let mocksrv = fakeserver.run(scenario);
        reconcile_echo(Arc::new(echo), testctx)
            .await
            .expect("reconciler");
        timeout_after_1s(mocksrv).await;
    }
}

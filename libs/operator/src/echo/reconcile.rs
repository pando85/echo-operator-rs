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
use kube::runtime::finalizer::{finalizer, Event};
use kube::ResourceExt;
use serde_json::json;
use tokio::time::Duration;
use tracing::{field, info, instrument, Span};

pub static ECHO_FINALIZER: &str = "echoes.example.com";

async fn patch(client: Client, echo: &Echo) -> Result<Deployment, Error> {
    let deployment_api: Api<Deployment> = Api::namespaced(client, &echo.get_namespace()?);
    let owner_references = echo.controller_owner_ref(&()).map(|oref| vec![oref]);

    let mut labels: BTreeMap<String, String> = BTreeMap::new();
    labels.insert("app".to_owned(), echo.name_any());
    let deployment = Deployment {
        metadata: ObjectMeta {
            name: Some(echo.name_any()),
            namespace: Some(echo.get_namespace()?),
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
    fn get_namespace(&self) -> Result<String> {
        self.namespace()
            .ok_or_else(|| Error::MissingObjectKey(".metadata.name"))
    }

    async fn update_replicas(&self, replicas: i32, ctx: Arc<Context>) -> Result<()> {
        let echo: Api<Echo> = Api::namespaced(ctx.client.clone(), &self.get_namespace()?);
        let new_status = Patch::Apply(json!({
            "apiVersion": "example.com/v1",
            "kind": "Echo",
            "status": EchoStatus {
                replicas: Some(replicas),
                ..EchoStatus::default()
            }
        }));
        let patch = PatchParams::apply("echoes.example.com").force();
        let _o = echo
            .patch_status(&self.name_any(), &patch, &new_status)
            .await
            .map_err(Error::KubeError)?;
        Ok(())
    }

    async fn reconcile(&self, ctx: Arc<Context>) -> Result<()> {
        patch(ctx.client.clone(), self).await?;
        self.update_replicas(self.spec.replicas, ctx).await?;
        Ok(())
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

    let echoes = Api::<Echo>::namespaced(ctx.client.clone(), &namespace);

    finalizer(&echoes, ECHO_FINALIZER, echo, |event| async {
        match event {
            Event::Apply(echo) => {
                echo.reconcile(ctx.clone()).await?;
                Ok(Action::requeue(Duration::from_secs(5 * 60)))
            }
            Event::Cleanup(_) => {
                // Kubernetes handles deletions
                Ok(Action::requeue(Duration::from_secs(5 * 60)))
            }
        }
    })
    .await
    .map_err(|e| Error::FinalizerError(Box::new(e)))
}

#[cfg(test)]
mod test {
    use super::reconcile;

    use crate::controller::{Context, State};
    use crate::crd::echo::Echo;
    use crate::echo::test::{timeout_after_1s, Scenario};
    use std::sync::Arc;

    #[tokio::test]
    async fn echoes_without_finalizer_gets_a_finalizer() {
        let (testctx, fakeserver) = Context::test();
        let echo = Echo::test();
        let mocksrv = fakeserver.run(Scenario::FinalizerCreation(echo.clone()));
        reconcile(Arc::new(echo), testctx)
            .await
            .expect("reconciler");
        timeout_after_1s(mocksrv).await;
    }

    #[tokio::test]
    async fn finalized_echo_create() {
        let (testctx, fakeserver) = Context::test();
        let echo = Echo::test().finalized();
        let mocksrv = fakeserver.run(Scenario::EchoPatch(echo.clone()));
        reconcile(Arc::new(echo), testctx)
            .await
            .expect("reconciler");
        timeout_after_1s(mocksrv).await;
    }

    #[tokio::test]
    async fn finalized_echo_causes_status_patch() {
        let (testctx, fakeserver) = Context::test();
        let echo = Echo::test_with_status().finalized();
        let mocksrv = fakeserver.run(Scenario::EchoPatch(echo.clone()));
        reconcile(Arc::new(echo), testctx)
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
        reconcile(Arc::new(echo), testctx)
            .await
            .expect("reconciler");
        timeout_after_1s(mocksrv).await;
    }

    #[tokio::test]
    async fn finalized_echo_with_delete_timestamp_causes_delete() {
        let (testctx, fakeserver) = Context::test();
        let echo = Echo::test().finalized().needs_delete();
        let mocksrv = fakeserver.run(Scenario::Cleanup(echo.clone()));
        reconcile(Arc::new(echo), testctx)
            .await
            .expect("reconciler");
        timeout_after_1s(mocksrv).await;
    }

    // #[tokio::test]
    // async fn illegal_echo_reconcile_errors_which_bumps_failure_metric() {
    //     let (testctx, fakeserver) = Context::test();
    //     let echo = Arc::new(Echo::illegal().finalized());
    //     let mocksrv = fakeserver.run(Scenario::RadioSilence);
    //     let res = reconcile(echo.clone(), testctx.clone()).await;
    //     timeout_after_1s(mocksrv).await;
    //     assert!(res.is_err(), "apply reconciler fails on illegal doc");
    //     let err = res.unwrap_err();
    //     dbg!(&err);
    //     assert!(err.to_string().contains("IllegalEcho"));
    //     // calling error policy with the reconciler error should cause the correct metric to be set
    //     error_policy(echo.clone(), &err, testctx.clone());
    //     let err_labels = ErrorLabels {
    //         instance: "illegal".into(),
    //         error: "finalizererror(applyfailed(illegaldocument))".into(),
    //     };
    //     let metrics = &testctx.metrics.reconcile;
    //     let failures = metrics.failures.get_or_create(&err_labels).get();
    //     assert_eq!(failures, 1);
    // }

    // Integration test without mocks
    use kube::api::{Api, ListParams, Patch, PatchParams};
    #[tokio::test]
    #[ignore = "uses k8s current-context"]
    async fn integration_reconcile_should_set_status_and_send_event() {
        let client = kube::Client::try_default().await.unwrap();
        let ctx = State::default().to_context(client.clone());

        // create a test doc
        let echo = Echo::test().finalized().change_replicas(3);
        let docs: Api<Echo> = Api::namespaced(client.clone(), "default");
        let ssapply = PatchParams::apply("ctrltest");
        let patch = Patch::Apply(echo.clone());
        docs.patch("test", &ssapply, &patch).await.unwrap();

        // reconcile it (as if it was just applied to the cluster like this)
        reconcile(Arc::new(echo), ctx).await.unwrap();

        // verify side-effects happened
        let output = docs.get_status("test").await.unwrap();
        assert!(output.status.is_some());
        // verify hide event was found
        let events: Api<k8s_openapi::api::core::v1::Event> = Api::all(client.clone());
        let opts =
            ListParams::default().fields("involvedObject.kind=Echo,involvedObject.name=test");
        let event = events
            .list(&opts)
            .await
            .unwrap()
            .into_iter()
            .filter(|e| e.reason.as_deref() == Some("HideRequested"))
            .last()
            .unwrap();
        dbg!("got ev: {:?}", &event);
        assert_eq!(event.action.as_deref(), Some("Hiding"));
    }
}

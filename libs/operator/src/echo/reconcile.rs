use crate::controller::Context;
use crate::crd::echo::{Echo, EchoStatus};
use crate::error::{Error, Result};
use crate::telemetry;

use std::collections::BTreeMap;
use std::sync::Arc;

use chrono::Utc;
use k8s_openapi::api::apps::v1::{Deployment, DeploymentSpec, DeploymentStatus};
use k8s_openapi::api::core::v1::{Container, ContainerPort, PodSpec, PodTemplateSpec};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{Condition, LabelSelector, Time};
use kube::api::{Api, ObjectMeta, Patch, PatchParams, Resource};
use kube::client::Client;
use kube::runtime::controller::Action;
use kube::runtime::reflector::ObjectRef;
use kube::ResourceExt;
use serde_json::json;
use tokio::time::Duration;
use tracing::{debug, field, info, instrument, trace, Span};

static STATUS_READY: &str = "Ready";
static STATUS_PROGRESSING: &str = "Progressing";

#[instrument(skip(ctx, echo))]
pub async fn reconcile_echo(echo: Arc<Echo>, ctx: Arc<Context<Deployment>>) -> Result<Action> {
    let trace_id = telemetry::get_trace_id();
    Span::current().record("trace_id", field::display(&trace_id));
    let _timer = ctx.metrics.reconcile_count_and_measure(&trace_id);
    info!(msg = "reconciling Echo");

    let _ignore_errors = echo.update_status(ctx.clone()).await.map_err(|e| {
        debug!(msg = "failed to reconcile status", %e);
        ctx.metrics.status_update_errors_inc();
    });
    echo.patch(ctx).await?;
    Ok(Action::requeue(Duration::from_secs(5 * 60)))
}

impl Echo {
    #[inline]
    fn get_namespace(&self) -> String {
        // safe unwrap: Echo is namespaced scoped
        self.namespace().unwrap()
    }

    async fn patch(&self, ctx: Arc<Context<Deployment>>) -> Result<Deployment, Error> {
        let namespace = self.get_namespace();
        let deployment_api = Api::<Deployment>::namespaced(ctx.client.clone(), &namespace);
        let owner_references = self.controller_owner_ref(&()).map(|oref| vec![oref]);

        let name = self.name_any();
        let labels: BTreeMap<String, String> = self
            .labels()
            .iter()
            .map(|(k, v)| (k.to_owned(), v.to_owned()))
            .chain([
                ("app".to_owned(), name.clone()),
                ("app.kubernetes.io/name".to_owned(), "echo".to_owned()),
                (
                    "app.kubernetes.io/managed-by".to_owned(),
                    "echo-operator".to_owned(),
                ),
            ])
            .collect();

        ctx.metrics
            .spec_replicas_set(&namespace, &name, self.spec.replicas);
        let deployment = Deployment {
            metadata: ObjectMeta {
                name: Some(self.name_any()),
                namespace: Some(namespace),
                labels: Some(labels.clone()),
                owner_references,
                ..ObjectMeta::default()
            },
            spec: Some(DeploymentSpec {
                replicas: Some(self.spec.replicas),
                selector: LabelSelector {
                    match_expressions: None,
                    match_labels: Some(labels.clone()),
                },
                template: PodTemplateSpec {
                    spec: Some(PodSpec {
                        containers: vec![Container {
                            name: self.name_any(),
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

        let result = deployment_api
            .patch(
                &self.name_any(),
                &PatchParams::apply("echoes.example.com").force(),
                &Patch::Apply(&deployment),
            )
            .await;
        match result {
            Ok(deployment) => Ok(deployment),
            Err(e) => {
                match e {
                    kube::Error::Api(ae) if ae.code == 422 => {
                        info!(msg = "recreating Deployment because the update operation wasn't possible", reason=ae.reason);
                        self.delete_deployment(ctx.client.clone()).await?;
                        ctx.metrics.reconcile_deploy_delete_create_inc();
                        deployment_api
                            .patch(
                                &self.name_any(),
                                &PatchParams::apply("echoes.example.com").force(),
                                &Patch::Apply(&deployment),
                            )
                            .await
                            .map_err(Error::KubeError)
                    }
                    _ => Err(Error::KubeError(e)),
                }
            }
        }
    }

    async fn delete_deployment(&self, client: Client) -> Result<(), Error> {
        let deployment_api = Api::<Deployment>::namespaced(client, &self.get_namespace());
        deployment_api
            .delete(&self.name_any(), &Default::default())
            .await
            .map_err(Error::KubeError)?;
        Ok(())
    }

    async fn update_status(&self, ctx: Arc<Context<Deployment>>) -> Result<()> {
        let namespace = &self.get_namespace();
        let deployment_ref =
            ObjectRef::<Deployment>::new_with(&self.name_any(), ()).within(namespace);
        debug!(msg = "getting deployment");
        let deployment = ctx
            .store
            .get(&deployment_ref)
            .ok_or_else(|| Error::MissingObject("deployment"))?;
        let owner = deployment
            .metadata
            .owner_references
            .as_ref()
            .and_then(|refs| refs.iter().find(|r| r.controller == Some(true)))
            .ok_or_else(|| Error::MissingObjectKey("ownerReferences"))?;

        let deployment_status = deployment
            .status
            .as_ref()
            .ok_or_else(|| Error::MissingObjectKey("status"))?;

        let new_status = self.generate_status(deployment_status, deployment.metadata.generation);

        let new_status_patch = Patch::Apply(json!({
            "apiVersion": "example.com/v1",
            "kind": "Echo",
            "status": new_status
        }));
        debug!(msg = "updating Echo status");
        trace!(msg = format!("new status {:?}", new_status_patch));
        let patch = PatchParams::apply("echoes.example.com").force();
        let echo_api = Api::<Echo>::namespaced(ctx.client.clone(), namespace);
        let _o = echo_api
            .patch_status(&owner.name, &patch, &new_status_patch)
            .await
            .map_err(Error::KubeError)?;
        Ok(())
    }

    /// Generate the EchoStatus based on the deployment status
    fn generate_status(
        &self,
        deployment_status: &DeploymentStatus,
        deployment_metadata_generation: Option<i64>,
    ) -> EchoStatus {
        let status_type = Echo::determine_status_type(deployment_status);

        // Create a new condition with the current status
        let new_condition = Condition {
            type_: status_type.to_string(),
            status: "True".to_string(),
            reason: "".to_string(),
            message: "".to_string(),
            last_transition_time: Time(Utc::now()),
            observed_generation: deployment_metadata_generation,
        };

        let conditions = self.update_conditions(&new_condition, status_type);

        EchoStatus {
            available_replicas: deployment_status.available_replicas,
            observed_generation: deployment_metadata_generation,
            ready_replicas: deployment_status.ready_replicas,
            replicas: deployment_status.replicas,
            updated_replicas: deployment_status.updated_replicas,
            conditions: Some(conditions),
        }
    }

    /// Determine the status type based on the deployment status
    fn determine_status_type(deployment_status: &DeploymentStatus) -> &str {
        if deployment_status.replicas == deployment_status.updated_replicas
            && deployment_status.replicas == deployment_status.ready_replicas
        {
            STATUS_READY
        } else {
            STATUS_PROGRESSING
        }
    }

    /// Update conditions based on the current status and previous conditions in the Echo
    fn update_conditions(&self, new_condition: &Condition, status_type: &str) -> Vec<Condition> {
        match self.status.as_ref().and_then(|s| s.conditions.as_ref()) {
            // Remove the 'Ready' condition if we are 'Progressing'
            Some(previous_conditions) if status_type == STATUS_PROGRESSING => previous_conditions
                .iter()
                .filter(|c| c.type_ != STATUS_READY)
                .cloned()
                .chain(std::iter::once(new_condition.clone()))
                .collect(),

            // Add the new condition if it's not already present
            Some(previous_conditions)
                if !previous_conditions.iter().any(|c| c.type_ == *status_type) =>
            {
                previous_conditions
                    .iter()
                    .cloned()
                    .chain(std::iter::once(new_condition.clone()))
                    .collect()
            }

            // Otherwise, keep the existing conditions unchanged
            Some(previous_conditions) => previous_conditions.clone(),

            // No previous conditions; start fresh with the new condition
            None => vec![new_condition.clone()],
        }
    }
}

#[cfg(test)]
mod test {
    use super::{reconcile_echo, Echo, STATUS_PROGRESSING, STATUS_READY};

    use crate::controller::Context;
    use crate::crd::echo::EchoStatus;
    use crate::echo::test::{timeout_after_1s, Scenario};

    use std::sync::Arc;

    use chrono::Utc;
    use k8s_openapi::api::apps::v1::DeploymentStatus;
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::{Condition, Time};

    #[tokio::test]
    async fn echo_create() {
        let (testctx, fakeserver) = Context::test();
        let echo = Echo::test(None);
        let mocksrv = fakeserver.run(Scenario::EchoPatch(echo.clone()));
        reconcile_echo(Arc::new(echo), testctx)
            .await
            .expect("reconciler");
        timeout_after_1s(mocksrv).await;
    }

    #[tokio::test]
    async fn echo_causes_status_patch() {
        let (testctx, fakeserver) = Context::test();
        let echo = Echo::test(Some(EchoStatus::default()));
        let mocksrv = fakeserver.run(Scenario::EchoPatch(echo.clone()));
        reconcile_echo(Arc::new(echo), testctx)
            .await
            .expect("reconciler");
        timeout_after_1s(mocksrv).await;
    }

    #[tokio::test]
    async fn echo_with_replicas_causes_patch() {
        let (testctx, fakeserver) = Context::test();
        let echo = Echo::test(Some(EchoStatus::default())).change_replicas(3);
        let scenario = Scenario::EchoPatch(echo.clone());
        let mocksrv = fakeserver.run(scenario);
        reconcile_echo(Arc::new(echo), testctx)
            .await
            .expect("reconciler");
        timeout_after_1s(mocksrv).await;
    }

    #[test]
    fn test_generate_status_ready() {
        let deployment_status = DeploymentStatus {
            available_replicas: Some(3),
            ready_replicas: Some(3),
            replicas: Some(3),
            updated_replicas: Some(3),
            ..Default::default()
        };

        let deployment_metadata_generation = Some(1);
        let echo = Echo::test(None);

        let result = echo.generate_status(&deployment_status, deployment_metadata_generation);

        assert_eq!(result.available_replicas, Some(3));
        assert_eq!(result.ready_replicas, Some(3));
        assert_eq!(result.replicas, Some(3));
        assert_eq!(result.updated_replicas, Some(3));
        assert_eq!(result.observed_generation, Some(1));

        let conditions = result.conditions.unwrap();
        assert_eq!(conditions.len(), 1);
        assert_eq!(conditions[0].type_, STATUS_READY);
    }

    #[test]
    fn test_generate_status_progressing() {
        let deployment_status = DeploymentStatus {
            available_replicas: Some(2),
            ready_replicas: Some(2),
            replicas: Some(3),
            updated_replicas: Some(2),
            ..Default::default()
        };

        let deployment_metadata_generation = Some(2);
        let echo = Echo::test(None);

        let result = echo.generate_status(&deployment_status, deployment_metadata_generation);

        assert_eq!(result.available_replicas, Some(2));
        assert_eq!(result.ready_replicas, Some(2));
        assert_eq!(result.replicas, Some(3));
        assert_eq!(result.updated_replicas, Some(2));
        assert_eq!(result.observed_generation, Some(2));

        let conditions = result.conditions.unwrap();
        assert_eq!(conditions.len(), 1);
        assert_eq!(conditions[0].type_, STATUS_PROGRESSING);
    }

    #[test]
    fn test_generate_status_add_new_condition() {
        let deployment_status = DeploymentStatus {
            available_replicas: Some(3),
            ready_replicas: Some(3),
            replicas: Some(3),
            updated_replicas: Some(3),
            ..Default::default()
        };

        let deployment_metadata_generation = Some(3);

        // Previous condition with a different type (Progressing)
        let previous_conditions = vec![Condition {
            type_: STATUS_PROGRESSING.to_string(),
            status: "True".to_string(),
            reason: "".to_string(),
            message: "".to_string(),
            last_transition_time: Time(Utc::now()),
            observed_generation: Some(1),
        }];

        let echo_status = EchoStatus {
            conditions: Some(previous_conditions),
            ..Default::default()
        };

        let echo = Echo::test(Some(echo_status));

        let result = echo.generate_status(&deployment_status, deployment_metadata_generation);

        let conditions = result.conditions.unwrap();
        assert_eq!(conditions.len(), 2);
        assert!(conditions.iter().any(|c| c.type_ == STATUS_READY));
        assert!(conditions.iter().any(|c| c.type_ == STATUS_PROGRESSING));
    }

    #[test]
    fn test_generate_status_replace_ready_condition() {
        let deployment_status = DeploymentStatus {
            available_replicas: Some(2),
            ready_replicas: Some(2),
            replicas: Some(3),
            updated_replicas: Some(2),
            ..Default::default()
        };

        let deployment_metadata_generation = Some(4);

        // Previous condition with type Ready
        let previous_conditions = vec![Condition {
            type_: STATUS_READY.to_string(),
            status: "True".to_string(),
            reason: "".to_string(),
            message: "".to_string(),
            last_transition_time: Time(Utc::now()),
            observed_generation: Some(2),
        }];

        let echo_status = EchoStatus {
            conditions: Some(previous_conditions),
            ..Default::default()
        };

        let echo = Echo::test(Some(echo_status));

        let result = echo.generate_status(&deployment_status, deployment_metadata_generation);

        let conditions = result.conditions.unwrap();
        assert_eq!(conditions.len(), 1);
        assert!(conditions.iter().all(|c| c.type_ == STATUS_PROGRESSING));
    }

    #[test]
    fn test_generate_status_no_previous_conditions() {
        let deployment_status = DeploymentStatus {
            available_replicas: Some(2),
            ready_replicas: Some(2),
            replicas: Some(3),
            updated_replicas: Some(2),
            ..Default::default()
        };

        let deployment_metadata_generation = Some(5);
        let echo = Echo::test(None);

        let result = echo.generate_status(&deployment_status, deployment_metadata_generation);

        let conditions = result.conditions.unwrap();
        assert_eq!(conditions.len(), 1);
        assert_eq!(conditions[0].type_, STATUS_PROGRESSING);
    }
}

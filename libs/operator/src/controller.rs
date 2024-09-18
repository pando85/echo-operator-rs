use crate::crd::echo::Echo;
use crate::echo;
use crate::error::{Error, Result};
use crate::finalizer;
use crate::metrics::Metrics;
use crate::telemetry;

use chrono::{DateTime, Utc};
use futures::StreamExt;
use kube::{
    api::{Api, ListParams, ResourceExt},
    client::Client,
    runtime::{
        controller::{Action, Controller},
        events::Reporter,
        watcher::Config,
    },
    Resource,
};
use serde::Serialize;
use std::sync::Arc;
use tokio::{sync::RwLock, time::Duration};
use tracing::*;

pub static ECHO_FINALIZER: &str = "echo.example.com";

// Context for our reconciler
#[derive(Clone)]
pub struct Context {
    /// Kubernetes client
    pub client: Client,
    /// Diagnostics read by the web server
    pub diagnostics: Arc<RwLock<Diagnostics>>,
    /// Prometheus metrics
    pub metrics: Arc<Metrics>,
}

#[instrument(skip(ctx, echo), fields(trace_id))]
async fn reconcile(echo: Arc<Echo>, ctx: Arc<Context>) -> Result<Action, Error> {
    let trace_id = telemetry::get_trace_id();
    Span::current().record("trace_id", field::display(&trace_id));

    let client: Client = ctx.client.clone(); // The `Client` is shared -> a clone from the reference is obtained

    let _timer = ctx.metrics.reconcile.count_and_measure(&trace_id);
    ctx.diagnostics.write().await.last_event = Utc::now();
    // The resource of `Echo` kind is required to have a namespace set. However, it is not guaranteed
    // the resource will have a `namespace` set. Therefore, the `namespace` field on object's metadata
    // is optional and Rust forces the programmer to check for it's existence first.
    let namespace: String = match echo.namespace() {
        None => {
            // If there is no namespace to deploy to defined, reconciliation ends with an error immediately.
            return Err(Error::UserInputError(
                "Expected Echo resource to be namespaced. Can't deploy to an unknown namespace."
                    .to_owned(),
            ));
        }
        // If namespace is known, proceed. In a more advanced version of the operator, perhaps
        // the namespace could be checked for existence first.
        Some(namespace) => namespace,
    };
    let name = echo.name_any(); // Name of the Echo resource is used to name the subresources as well.

    info!("Reconciling Echo \"{name}\" in {namespace}");

    // Performs action as decided by the `determine_action` function.
    match determine_action(&echo) {
        EchoAction::Create => {
            // Creates a deployment with `n` Echo service pods, but applies a finalizer first.
            // Finalizer is applied first, as the operator might be shut down and restarted
            // at any time, leaving subresources in intermediate state. This prevents leaks on
            // the `Echo` resource deletion.

            // Apply the finalizer first. If that fails, the `?` operator invokes automatic conversion
            // of `kube::Error` to the `Error` defined in this crate.
            finalizer::add(client.clone(), &name, &namespace)
                .await
                .map_err(Error::KubeError)?;
            // Invoke creation of a Kubernetes built-in resource named deployment with `n` echo service pods.
            echo::deploy(client, &name, echo.spec.replicas, &namespace).await?;
            Ok(Action::requeue(Duration::from_secs(10)))
        }
        EchoAction::Delete => {
            // Deletes any subresources related to this `Echo` resources. If and only if all subresources
            // are deleted, the finalizer is removed and Kubernetes is free to remove the `Echo` resource.

            //First, delete the deployment. If there is any error deleting the deployment, it is
            // automatically converted into `Error` defined in this crate and the reconciliation is ended
            // with that error.
            // Note: A more advanced implementation would check for the Deployment's existence.
            echo::delete(client.clone(), &name, &namespace).await?;

            // Once the deployment is successfully removed, remove the finalizer to make it possible
            // for Kubernetes to delete the `Echo` resource.
            finalizer::delete(client, &name, &namespace)
                .await
                .map_err(Error::KubeError)?;
            Ok(Action::await_change()) // Makes no sense to delete after a successful delete, as the resource is gone
        }
        // The resource is already in desired state, do nothing and re-check after 10 seconds
        EchoAction::NoOp => Ok(Action::requeue(Duration::from_secs(10))),
    }
}

fn error_policy(echo: Arc<Echo>, error: &Error, ctx: Arc<Context>) -> Action {
    warn!("reconcile failed: {:?}", error);
    ctx.metrics.reconcile.set_failure(&echo, error);
    Action::requeue(Duration::from_secs(5 * 60))
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

/// Resources arrives into reconciliation queue in a certain state. This function looks at
/// the state of given `Echo` resource and decides which actions needs to be performed.
/// The finite set of possible actions is represented by the `EchoAction` enum.
///
/// # Arguments
/// - `echo`: A reference to `Echo` being reconciled to decide next action upon.
fn determine_action(echo: &Echo) -> EchoAction {
    if echo.meta().deletion_timestamp.is_some() {
        EchoAction::Delete
    } else if echo
        .meta()
        .finalizers
        .as_ref()
        .map_or(true, |finalizers| finalizers.is_empty())
    {
        EchoAction::Create
    } else {
        EchoAction::NoOp
    }
}

/// Diagnostics to be exposed by the web server
#[derive(Clone, Serialize)]
pub struct Diagnostics {
    #[serde(deserialize_with = "from_ts")]
    pub last_event: DateTime<Utc>,
    #[serde(skip)]
    pub reporter: Reporter,
}
impl Default for Diagnostics {
    fn default() -> Self {
        Self {
            last_event: Utc::now(),
            reporter: "echo-controller".into(),
        }
    }
}

/// State shared between the controller and the web server
#[derive(Clone, Default)]
pub struct State {
    /// Diagnostics populated by the reconciler
    diagnostics: Arc<RwLock<Diagnostics>>,
    /// Metrics
    metrics: Arc<Metrics>,
}

/// State wrapper around the controller outputs for the web server
impl State {
    /// Metrics getter
    pub fn metrics(&self) -> String {
        let mut buffer = String::new();
        let registry = &*self.metrics.registry;
        prometheus_client::encoding::text::encode(&mut buffer, registry).unwrap();
        buffer
    }

    /// State getter
    pub async fn diagnostics(&self) -> Diagnostics {
        self.diagnostics.read().await.clone()
    }

    // Create a Controller Context that can update State
    pub fn to_context(&self, client: Client) -> Arc<Context> {
        Arc::new(Context {
            client,
            metrics: self.metrics.clone(),
            diagnostics: self.diagnostics.clone(),
        })
    }
}

/// Initialize the controller and shared state (given the crd is installed)
pub async fn run(state: State) {
    let client = Client::try_default()
        .await
        .expect("failed to create kube Client");
    let echoes = Api::<Echo>::all(client.clone());
    if let Err(e) = echoes.list(&ListParams::default().limit(1)).await {
        error!("CRD is not queryable; {e:?}. Is the CRD installed?");
        info!("Installation: cargo run --bin crdgen | kubectl apply -f -");
        std::process::exit(1);
    }
    trace!("Init controller");
    Controller::new(echoes, Config::default().any_semantic())
        .shutdown_on_signal()
        .run(reconcile, error_policy, state.to_context(client))
        .filter_map(|x| async move { std::result::Result::ok(x) })
        .for_each(|_| futures::future::ready(()))
        .await;
}

use crate::controller::{Context, State};
use crate::crd::echo::Echo;
use crate::echo::reconcile::reconcile;
use crate::error::Error;

use futures::StreamExt;
use kube::api::{Api, ListParams};
use kube::client::Client;
use kube::runtime::controller::{Action, Controller};
use kube::runtime::watcher::Config;
use std::sync::Arc;
use tokio::time::Duration;
use tracing::*;

pub fn error_policy(echo: Arc<Echo>, error: &Error, ctx: Arc<Context>) -> Action {
    warn!("reconcile failed: {:?}", error);
    ctx.metrics.reconcile.set_failure(&echo, error);
    Action::requeue(Duration::from_secs(5 * 60))
}

/// Initialize echoes controller and shared state (given the crd is installed)
pub async fn run(state: State) {
    let client = Client::try_default()
        .await
        .expect("failed to create kube Client");
    let echoes = Api::<Echo>::all(client.clone());
    if let Err(e) = echoes.list(&ListParams::default().limit(1)).await {
        error!("CRD is not queryable; {e:?}. Is the CRD installed?");
        std::process::exit(1);
    }

    info!("Starting echoes controller");
    Controller::new(echoes, Config::default().any_semantic())
        .shutdown_on_signal()
        .run(reconcile, error_policy, state.to_context(client))
        .filter_map(|x| async move { std::result::Result::ok(x) })
        .for_each(|_| futures::future::ready(()))
        .await;
}

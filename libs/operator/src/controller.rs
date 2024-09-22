use crate::metrics::Metrics;

use kube::client::Client;
use std::sync::Arc;

// Context for our reconciler
#[derive(Clone)]
pub struct Context {
    /// Kubernetes client
    pub client: Client,
    /// Prometheus metrics
    pub metrics: Arc<Metrics>,
}

/// State shared between the controller and the web server
#[derive(Clone, Default)]
pub struct State {
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

    /// Create a Controller Context that can update State
    pub fn to_context(&self, client: Client) -> Arc<Context> {
        Arc::new(Context {
            client,
            metrics: self.metrics.clone(),
        })
    }
}

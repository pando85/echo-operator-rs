use crate::metrics::Metrics;

use std::hash::Hash;
use std::sync::Arc;

use kube::client::Client;
use kube::runtime::reflector::{Lookup, Store};

// Context for our reconciler
#[derive(Clone)]
pub struct Context<K: 'static + Lookup>
where
    K::DynamicType: Hash + Eq,
{
    /// Kubernetes client
    pub client: Client,
    /// Prometheus metrics
    pub metrics: Arc<Metrics>,
    /// Shared store
    pub store: Arc<Store<K>>,
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
    pub fn to_context<K: 'static + Lookup>(
        &self,
        client: Client,
        store: Store<K>,
    ) -> Arc<Context<K>>
    where
        K::DynamicType: Hash + Eq,
    {
        Arc::new(Context {
            client,
            metrics: self.metrics.clone(),
            store: Arc::new(store),
        })
    }
}

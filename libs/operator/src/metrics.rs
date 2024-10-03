use crate::error::Error;

use kube::ResourceExt;
use opentelemetry::trace::TraceId;
use prometheus_client::encoding::EncodeLabelSet;
use prometheus_client::metrics::{
    counter::Counter, exemplar::HistogramWithExemplars, family::Family,
};
use prometheus_client::registry::{Registry, Unit};
use std::sync::Arc;
use tokio::time::Instant;

#[derive(Clone)]
pub struct Metrics {
    pub reconcile: ReconcileMetrics,
    pub registry: Arc<Registry>,
}

impl Default for Metrics {
    fn default() -> Self {
        let mut registry = Registry::with_prefix("kaniop_reconcile");
        let reconcile = ReconcileMetrics::default().register(&mut registry);
        Self {
            registry: Arc::new(registry),
            reconcile,
        }
    }
}

#[derive(Clone, Hash, PartialEq, Eq, EncodeLabelSet, Debug, Default)]
pub struct TraceLabel {
    pub id: String,
}
impl TryFrom<&TraceId> for TraceLabel {
    type Error = Error;

    fn try_from(id: &TraceId) -> Result<TraceLabel, Self::Error> {
        if std::matches!(id, &TraceId::INVALID) {
            Err(Error::InvalidTraceId)
        } else {
            let trace_id = id.to_string();
            Ok(Self { id: trace_id })
        }
    }
}

#[derive(Clone)]
pub struct ReconcileMetrics {
    pub runs: Family<(), Counter>,
    pub failures: Family<ErrorLabels, Counter>,
    pub duration: HistogramWithExemplars<TraceLabel>,
}

impl Default for ReconcileMetrics {
    fn default() -> Self {
        Self {
            runs: Family::<(), Counter>::default(),
            failures: Family::<ErrorLabels, Counter>::default(),
            duration: HistogramWithExemplars::new([0.1, 0.5, 1., 5., 10.].into_iter()),
        }
    }
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct ErrorLabels {
    pub instance: String,
    pub error: String,
}

impl ReconcileMetrics {
    /// Register API metrics to start tracking them.
    pub fn register(self, r: &mut Registry) -> Self {
        r.register_with_unit(
            "duration",
            "reconcile duration",
            Unit::Seconds,
            self.duration.clone(),
        );
        r.register("failures", "reconciliation errors", self.failures.clone());
        r.register("runs", "reconciliations", self.runs.clone());
        self
    }

    pub fn set_failure<K: ResourceExt>(&self, obj: &Arc<K>, e: &Error) {
        self.failures
            .get_or_create(&ErrorLabels {
                instance: obj.name_any(),
                error: e.metric_label(),
            })
            .inc();
    }

    pub fn count_and_measure(&self, trace_id: &TraceId) -> ReconcileMeasurer {
        self.runs.get_or_create(&()).inc();
        ReconcileMeasurer {
            start: Instant::now(),
            labels: trace_id.try_into().ok(),
            metric: self.duration.clone(),
        }
    }
}

/// Smart function duration measurer
///
/// Relies on Drop to calculate duration and register the observation in the histogram
pub struct ReconcileMeasurer {
    start: Instant,
    labels: Option<TraceLabel>,
    metric: HistogramWithExemplars<TraceLabel>,
}

impl Drop for ReconcileMeasurer {
    fn drop(&mut self) {
        let duration = self.start.elapsed().as_secs_f64();
        let labels = self.labels.take();
        self.metric.observe(duration, labels);
    }
}

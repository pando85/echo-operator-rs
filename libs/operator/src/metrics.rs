use crate::error::Error;

use opentelemetry::trace::TraceId;
use prometheus_client::encoding::EncodeLabelSet;
use prometheus_client::metrics::{
    counter::Counter, exemplar::HistogramWithExemplars, family::Family,
};
use prometheus_client::registry::{Registry, Unit};
use std::sync::Arc;
use tokio::time::Instant;

use crate::controller::ControllerId;
use std::collections::HashMap;

#[derive(Clone)]
pub struct Metrics {
    pub controllers: HashMap<ControllerId, Arc<ControllerMetrics>>,
    pub registry: Arc<Registry>,
}

impl Metrics {
    pub fn new(mut registry: Registry, controller_names: &[&'static str]) -> Self {
        let controllers = controller_names
            .iter()
            .map(|&id| {
                let reconcile = ReconcileMetrics::new(id).register(&mut registry);
                let controller_metrics = ControllerMetrics { reconcile };
                (id, Arc::new(controller_metrics))
            })
            .collect::<HashMap<ControllerId, Arc<ControllerMetrics>>>();

        Self {
            registry: Arc::new(registry),
            controllers,
        }
    }
}

#[derive(Default)]
pub struct ControllerMetrics {
    pub reconcile: ReconcileMetrics,
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
    controller: String,
    pub runs: Family<ControllerLabels, Counter>,
    pub failures: Family<ErrorLabels, Counter>,
    pub duration: Family<ControllerLabels, HistogramWithExemplars<TraceLabel>>,
}

impl ReconcileMetrics {
    pub fn new(controller: &str) -> Self {
        Self {
            controller: controller.to_string(),
            runs: Family::<ControllerLabels, Counter>::default(),
            failures: Family::<ErrorLabels, Counter>::default(),
            duration:
                Family::<ControllerLabels, HistogramWithExemplars<TraceLabel>>::new_with_constructor(
                    || HistogramWithExemplars::new([0.1, 0.5, 1., 5., 10.].into_iter()),
                ),
        }
    }
}

impl Default for ReconcileMetrics {
    fn default() -> Self {
        ReconcileMetrics::new("controller_default")
    }
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct ControllerLabels {
    pub controller: String,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct ErrorLabels {
    pub controller: String,
    pub error: String,
}

impl ReconcileMetrics {
    /// Register API metrics to start tracking them.
    pub fn register(self, r: &mut Registry) -> Self {
        r.register_with_unit(
            "reconcile_duration",
            "reconcile duration",
            Unit::Seconds,
            self.duration.clone(),
        );
        r.register(
            "reconcile_failures",
            "reconciliation errors",
            self.failures.clone(),
        );
        r.register("reconcile_runs", "reconciliations", self.runs.clone());
        self
    }

    pub fn set_failure(&self, e: &Error) {
        self.failures
            .get_or_create(&ErrorLabels {
                controller: self.controller.clone(),
                error: e.metric_label(),
            })
            .inc();
    }

    pub fn count_and_measure(&self, trace_id: &TraceId) -> ReconcileMeasurer {
        let controller_labels = ControllerLabels {
            controller: self.controller.clone(),
        };
        self.runs.get_or_create(&controller_labels).inc();
        ReconcileMeasurer {
            start: Instant::now(),
            labels: trace_id.try_into().ok(),
            metric: self.duration.get_or_create(&controller_labels).clone(),
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

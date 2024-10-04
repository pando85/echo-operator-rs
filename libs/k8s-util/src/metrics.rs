use crate::url::template_path;

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use futures::future::FutureExt;
use http::Request;
use prometheus_client::encoding::EncodeLabelSet;
use prometheus_client::metrics::{family::Family, histogram::Histogram};
use prometheus_client::registry::Registry;
use tokio::time::Instant;
use tower::{Layer, Service};

#[derive(Clone, Hash, PartialEq, Eq, EncodeLabelSet, Debug, Default)]
pub struct EndpointLabel {
    pub endpoint: String,
}

pub struct MetricsLayer {
    request_histogram: Family<EndpointLabel, Histogram>,
}

impl MetricsLayer {
    pub fn new(registry: &mut Registry) -> Self {
        // TODO: remove bucket, implement summary (without quantiles):
        // https://github.com/prometheus/client_rust/pull/67
        let request_histogram: Family<EndpointLabel, Histogram> =
            Family::new_with_constructor(|| Histogram::new([].into_iter()));

        // TODO: add Counter for all requests with status code
        registry.register(
            "kubernetes_client",
            "Summary of latencies for the Kubernetes client's requests by endpoint",
            request_histogram.clone(),
        );

        Self {
            request_histogram: request_histogram.clone(),
        }
    }
}

impl<S> Layer<S> for MetricsLayer {
    type Service = MetricsService<S>;

    fn layer(&self, inner: S) -> Self::Service {
        MetricsService {
            inner,
            request_histogram: self.request_histogram.clone(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct MetricsService<S> {
    inner: S,
    request_histogram: Family<EndpointLabel, Histogram>,
}

impl<S, ReqBody> Service<Request<ReqBody>> for MetricsService<S>
where
    S: Service<Request<ReqBody>>,
    S::Future: Send + 'static,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, req: Request<ReqBody>) -> Self::Future {
        let path_template = template_path(req.uri().path(), None);
        let labels = EndpointLabel {
            endpoint: url_escape::encode_path(&path_template).to_string(),
        };

        let start_time = Instant::now();

        let fut = self.inner.call(req);
        let request_histogram = self.request_histogram.clone();
        async move {
            let result = fut.await;
            let duration = start_time.elapsed().as_secs_f64();
            request_histogram.get_or_create(&labels).observe(duration);
            result
        }
        .boxed()
    }
}

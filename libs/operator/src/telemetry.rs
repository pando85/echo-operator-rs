use opentelemetry::trace::TraceId;
use tonic::transport::Channel;
use tracing_subscriber::{prelude::*, EnvFilter, Registry};

///  Fetch an opentelemetry::trace::TraceId as hex through the full tracing stack
pub fn get_trace_id() -> TraceId {
    use opentelemetry::trace::TraceContextExt as _; // opentelemetry::Context -> opentelemetry::trace::Span
    use tracing_opentelemetry::OpenTelemetrySpanExt as _; // tracing::Span to opentelemetry::Context

    tracing::Span::current()
        .context()
        .span()
        .span_context()
        .trace_id()
}

async fn init_tracer() -> opentelemetry::sdk::trace::Tracer {
    let otlp_endpoint = std::env::var("OPENTELEMETRY_ENDPOINT_URL")
        .expect("Need a otel tracing collector configured");

    let channel = Channel::from_shared(otlp_endpoint)
        .unwrap()
        .connect()
        .await
        .unwrap();

    opentelemetry_otlp::new_pipeline()
        .tracing()
        .with_exporter(
            opentelemetry_otlp::new_exporter()
                .tonic()
                .with_channel(channel),
        )
        .with_trace_config(opentelemetry::sdk::trace::config().with_resource(
            opentelemetry::sdk::Resource::new(vec![opentelemetry::KeyValue::new(
                "service.name",
                "doc-controller",
            )]),
        ))
        .install_batch(opentelemetry::runtime::Tokio)
        .unwrap()
}

/// Initialize loging and tracing
pub async fn init(tracing: bool) {
    let logger = tracing_subscriber::fmt::layer().compact();
    let env_filter = EnvFilter::try_from_default_env()
        .or(EnvFilter::try_new("info"))
        .unwrap();

    let collector = Registry::default().with(logger).with(env_filter);

    if tracing {
        let telemetry = tracing_opentelemetry::layer().with_tracer(init_tracer().await);
        tracing::subscriber::set_global_default(collector.with(telemetry)).unwrap();
    } else {
        tracing::subscriber::set_global_default(collector).unwrap();
    }
}

#[cfg(test)]
mod test {
    // This test only works when telemetry is initialized fully
    // and requires OPENTELEMETRY_ENDPOINT_URL pointing to a valid server
    #[tokio::test]
    #[ignore = "requires a trace exporter"]
    async fn get_trace_id_returns_valid_traces() {
        use super::*;
        super::init(true).await;
        #[tracing::instrument(name = "test_span")] // need to be in an instrumented fn
        fn test_trace_id() -> TraceId {
            get_trace_id()
        }
        assert_ne!(test_trace_id(), TraceId::INVALID, "valid trace");
    }
}

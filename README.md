# echo-operator-rs

**echo-operator-rs** is a fully-featured, opinionated Kubernetes operator example, written in Rust. This repository provides a robust and efficient CRD-based controller designed to streamline Kubernetes application management using a simple but powerful `echo` Custom Resource Definition (CRD).

## Why echo-operator-rs?

Built using the highly-performant [kube-rs](https://github.com/kube-rs/kube-rs) library, this operator exemplifies best practices for creating Rust-based Kubernetes operators. It offers a simple yet complete example, centered around an `echo` CRD that deploys a Kubernetes Deployment with a configurable number of replicas (`n`).

While `echo-operator-rs` is easy to understand and extend, it also brings a high level of sophistication to the table, featuring:

- **Minimal API Calls**: Every interaction with the Kubernetes API is optimized to reduce overhead, ensuring that your clusters stay responsive and resource-efficient.
- **Reflectors for Change Detection**: In Go operators, this would be called an "informers" In Rust, we've used `reflectors` to detect changes not only in CRDs but also in any created resources like Deployments. This guarantees that your operator is always in sync with the state of the cluster.
- **Backoff Pressure**: Intelligent backoff strategies are baked into the controller to handle resource contention gracefully, reducing failure retries and system pressure.

## Helm Chart Integration

Deploying `echo-operator-rs` is simple and fast, thanks to its Helm chart. The chart comes with predefined tests, making the deployment of your operator in any Kubernetes environment seamless.

To install the operator using Helm:

```bash
helm install echo-operator ./charts/echo-operator
```

## Testing

**echo-operator-rs** is designed for reliability and ease of development. It includes the following testing strategies:

- **Unit Tests**: To ensure each component works as expected.
- **Integration Tests**: To verify the operator works correctly with Kubernetes resources.
- **End-to-End (E2E) Tests**: Comprehensive tests covering the full operator lifecycle in a real Kubernetes cluster.

## Observability

Observability is a key feature of `echo-operator-rs`. It comes fully integrated with:

- **Distributed Tracing and Structured Logging**: Powered by the `opentelemetry-otlp` crate, tracing and logs provide real-time insights into the operator's behavior and interactions with the Kubernetes API.
- **Custom Metrics**: Metrics are implemented through the `prometheus-client` crate, offering critical insights into performance, errors, and resource management.

## Development Workflow

**echo-operator-rs** is designed with developer productivity in mind. Every operation in the development lifecycle, from formatting to testing, is managed through a simple `Makefile`. This includes:

- Building and running the operator
- Running unit, integration, and E2E tests
- Deploying using Helm

To see all available commands, just run:

```bash
make
```

## License

This project is licensed under the MIT License. See the [LICENSE.md](LICENSE.md) file for details.

pub mod controller;
pub mod reconcile;

#[cfg(test)]
mod test {
    use crate::controller::Context;
    use crate::crd::echo::{Echo, EchoSpec, EchoStatus};
    use crate::echo::reconcile::ECHO_FINALIZER;
    use crate::error::Result;

    use std::sync::Arc;

    use assert_json_diff::assert_json_include;
    use http::{Request, Response};
    use k8s_openapi::api::apps::v1::Deployment;
    use kube::{client::Body, Client, Resource, ResourceExt};
    use serde_json::json;

    impl Echo {
        /// This doesn't cause nothing
        // /// A echo that will cause the reconciler to fail
        // pub fn illegal() -> Self {
        //     let mut d = Echo::new("illegal", echo_spec_default());
        //     d.meta_mut().namespace = Some("default".into());
        //     d
        // }

        /// A non updated normal test echo
        pub fn test() -> Self {
            let mut e = Echo::new("test", EchoSpec { replicas: 1 });
            e.meta_mut().namespace = Some("default".into());
            e
        }

        /// An updated normal test echo
        pub fn test_with_status() -> Self {
            let mut e = Echo::new("test", EchoSpec { replicas: 1 });
            e.status = Some(EchoStatus {
                replicas: Some(1),
                ..EchoStatus::default()
            });
            e.meta_mut().namespace = Some("default".into());
            e
        }

        /// Modify echo to be set to hide
        pub fn change_replicas(mut self, replicas: i32) -> Self {
            self.spec.replicas = replicas;
            self
        }

        /// Modify echo to set a deletion timestamp
        pub fn needs_delete(mut self) -> Self {
            use chrono::prelude::{DateTime, TimeZone, Utc};
            let now: DateTime<Utc> = Utc.with_ymd_and_hms(2017, 4, 2, 12, 50, 32).unwrap();
            use k8s_openapi::apimachinery::pkg::apis::meta::v1::Time;
            self.meta_mut().deletion_timestamp = Some(Time(now));
            self
        }

        /// Modify a echo to have the expected finalizer
        pub fn finalized(mut self) -> Self {
            self.finalizers_mut().push(ECHO_FINALIZER.to_string());
            self
        }

        /// Modify a echo to have an expected status
        pub fn with_status(mut self, status: EchoStatus) -> Self {
            self.status = Some(status);
            self
        }
    }

    // We wrap tower_test::mock::Handle
    type ApiServerHandle = tower_test::mock::Handle<Request<Body>, Response<Body>>;
    pub struct ApiServerVerifier(ApiServerHandle);

    /// Scenarios we test for in ApiServerVerifier
    pub enum Scenario {
        /// objects without finalizers will get a finalizer applied (and not call the apply loop)
        FinalizerCreation(Echo),
        /// objects non reconciled will cause a create
        NonReconciledEchoCreate(Echo),
        /// object that is already reconciled
        NoOp(),
        /// finalized objects with hide set causes both an event and then a hide patch
        ChangeReplicasThenStatusPatch(Echo),
        /// finalized objects "with errors" (i.e. the "illegal" object) will short circuit the apply loop
        RadioSilence,
        /// objects with a deletion timestamp will run the cleanup loop sending event and removing the finalizer
        Cleanup(Echo),
    }

    pub async fn timeout_after_1s(handle: tokio::task::JoinHandle<()>) {
        tokio::time::timeout(std::time::Duration::from_secs(1), handle)
            .await
            .expect("timeout on mock apiserver")
            .expect("scenario succeeded")
    }

    impl ApiServerVerifier {
        /// Tests only get to run specific scenarios that has matching handlers
        ///
        /// This setup makes it easy to handle multiple requests by chaining handlers together.
        ///
        /// NB: If the controller is making more calls than we are handling in the scenario,
        /// you then typically see a `KubeError(Service(Closed(())))` from the reconciler.
        ///
        /// You should await the `JoinHandle` (with a timeout) from this function to ensure that the
        /// scenario runs to completion (i.e. all expected calls were responded to),
        /// using the timeout to catch missing api calls to Kubernetes.
        pub fn run(self, scenario: Scenario) -> tokio::task::JoinHandle<()> {
            tokio::spawn(async move {
                // moving self => one scenario per test
                match scenario {
                    Scenario::FinalizerCreation(echo) => self.handle_finalizer_creation(echo).await,
                    Scenario::NonReconciledEchoCreate(echo) => {
                        self.handle_echo_create(echo.clone())
                            .await
                            .unwrap()
                            .handle_status_patch(echo)
                            .await
                    }
                    Scenario::NoOp() => self.handle_do_nothing().await,
                    Scenario::ChangeReplicasThenStatusPatch(echo) => {
                        self.handle_echo_patch(echo.clone())
                            .await
                            .unwrap()
                            .handle_status_patch(echo)
                            .await
                    }
                    Scenario::RadioSilence => Ok(self),
                    Scenario::Cleanup(echo) => {
                        self.handle_echo_delete(echo.clone())
                            .await
                            .unwrap()
                            .handle_finalizer_removal(echo)
                            .await
                    }
                }
                .expect("scenario completed without errors");
            })
        }

        // chainable scenario handlers
        async fn handle_finalizer_creation(mut self, echo: Echo) -> Result<Self> {
            let (request, send) = self.0.next_request().await.expect("service not called");
            // We expect a json patch to the specified echo adding our finalizer
            assert_eq!(request.method(), http::Method::PATCH);
            assert_eq!(
                request.uri().to_string(),
                format!(
                    "/apis/example.com/v1/namespaces/default/echoes/{}?",
                    echo.name_any()
                )
            );
            let expected_patch = serde_json::json!([
                { "op": "test", "path": "/metadata/finalizers", "value": null },
                { "op": "add", "path": "/metadata/finalizers", "value": vec![ECHO_FINALIZER] }
            ]);
            let req_body = request.into_body().collect_bytes().await.unwrap();
            let runtime_patch: serde_json::Value =
                serde_json::from_slice(&req_body).expect("valid echo from runtime");
            assert_json_include!(actual: runtime_patch, expected: expected_patch);

            let response = serde_json::to_vec(&echo.finalized()).unwrap(); // respond as the apiserver would have
            send.send_response(Response::builder().body(Body::from(response)).unwrap());
            Ok(self)
        }

        async fn handle_finalizer_removal(mut self, echo: Echo) -> Result<Self> {
            let (request, send) = self.0.next_request().await.expect("service not called");
            // We expect a json patch to the specified echo removing our finalizer (at index 0)
            assert_eq!(request.method(), http::Method::PATCH);
            assert_eq!(
                request.uri().to_string(),
                format!(
                    "/apis/example.com/v1/namespaces/default/echoes/{}?",
                    echo.name_any()
                )
            );
            let expected_patch = serde_json::json!([
                { "op": "test", "path": "/metadata/finalizers/0", "value": ECHO_FINALIZER },
                { "op": "remove", "path": "/metadata/finalizers/0", "path": "/metadata/finalizers/0" }
            ]);
            let req_body = request.into_body().collect_bytes().await.unwrap();
            let runtime_patch: serde_json::Value =
                serde_json::from_slice(&req_body).expect("valid echo from runtime");
            assert_json_include!(actual: runtime_patch, expected: expected_patch);

            let response = serde_json::to_vec(&echo).unwrap(); // respond as the apiserver would have
            send.send_response(Response::builder().body(Body::from(response)).unwrap());
            Ok(self)
        }

        async fn handle_do_nothing(self) -> Result<Self> {
            Ok(self)
        }

        async fn handle_echo_create(mut self, echo: Echo) -> Result<Self> {
            let (request, send) = self.0.next_request().await.expect("service not called");
            assert_eq!(request.method(), http::Method::POST);
            assert_eq!(
                request.uri().to_string(),
                "/apis/apps/v1/namespaces/default/deployments?"
            );

            let req_body = request.into_body().collect_bytes().await.unwrap();
            let json: serde_json::Value =
                serde_json::from_slice(&req_body).expect("patch object is json");
            let deployment: Deployment = serde_json::from_value(json).expect("valid deployment");
            assert_eq!(
                deployment.clone().spec.unwrap().replicas.unwrap(),
                echo.spec.replicas,
                "deployment replicas equal to echo spec replicas"
            );
            let response = serde_json::to_vec(&deployment).unwrap();
            // pass through echo "patch accepted"
            send.send_response(Response::builder().body(Body::from(response)).unwrap());
            Ok(self)
        }

        async fn handle_echo_patch(mut self, echo: Echo) -> Result<Self> {
            let (request, send) = self.0.next_request().await.expect("service not called");
            assert_eq!(request.method(), http::Method::PATCH);
            assert_eq!(
                request.uri().to_string(),
                format!(
                    "/apis/apps/v1/namespaces/default/deployments/{}?",
                    echo.name_any()
                )
            );

            let req_body = request.into_body().collect_bytes().await.unwrap();
            let json: serde_json::Value =
                serde_json::from_slice(&req_body).expect("patch object is json");
            let deployment: Deployment = serde_json::from_value(json).expect("valid deployment");
            assert_eq!(
                deployment.clone().spec.unwrap().replicas.unwrap(),
                echo.spec.replicas,
                "deployment replicas equal to echo spec replicas"
            );
            let response = serde_json::to_vec(&deployment).unwrap();
            // pass through echo "patch accepted"
            send.send_response(Response::builder().body(Body::from(response)).unwrap());
            Ok(self)
        }

        async fn handle_echo_delete(mut self, echo: Echo) -> Result<Self> {
            let (request, send) = self.0.next_request().await.expect("service not called");
            assert_eq!(request.method(), http::Method::DELETE);
            assert_eq!(
                request.uri().to_string(),
                format!(
                    "/apis/apps/v1/namespaces/default/deployments/{}?",
                    echo.name_any()
                )
            );
            let req_body = request.into_body().collect_bytes().await.unwrap();
            let json: serde_json::Value =
                serde_json::from_slice(&req_body).expect("delete object is json");
            let expected = json!({});
            assert_eq!(json, expected);
            let response = serde_json::to_vec(&json).unwrap();
            // pass through echo "patch accepted"
            send.send_response(Response::builder().body(Body::from(response)).unwrap());
            Ok(self)
        }

        async fn handle_status_patch(mut self, echo: Echo) -> Result<Self> {
            let (request, send) = self.0.next_request().await.expect("service not called");
            assert_eq!(request.method(), http::Method::PATCH);
            assert_eq!(
                request.uri().to_string(),
                format!(
                    "/apis/example.com/v1/namespaces/default/echoes/{}/status?&force=true&fieldManager=kaniop",
                    echo.name_any()
                )
            );
            let req_body = request.into_body().collect_bytes().await.unwrap();
            let json: serde_json::Value =
                serde_json::from_slice(&req_body).expect("patch_status object is json");
            let status_json = json.get("status").expect("status object").clone();
            let status: EchoStatus = serde_json::from_value(status_json).expect("valid status");
            assert_eq!(
                status.replicas.unwrap(),
                echo.spec.replicas,
                "status.hidden iff echo.spec.hide"
            );
            let response = serde_json::to_vec(&echo.with_status(status)).unwrap();
            // pass through echo "patch accepted"
            send.send_response(Response::builder().body(Body::from(response)).unwrap());
            Ok(self)
        }
    }

    impl Context {
        // Create a test context with a mocked kube client, locally registered metrics and default diagnostics
        pub fn test() -> (Arc<Self>, ApiServerVerifier) {
            let (mock_service, handle) = tower_test::mock::pair::<Request<Body>, Response<Body>>();
            let mock_client = Client::new(mock_service, "default");
            let ctx = Self {
                client: mock_client,
                metrics: Arc::default(),
            };
            (Arc::new(ctx), ApiServerVerifier(handle))
        }
    }
}

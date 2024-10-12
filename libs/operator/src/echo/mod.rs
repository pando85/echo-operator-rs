pub mod controller;
pub mod reconcile;

#[cfg(test)]
mod test {
    use crate::controller::Context;
    use crate::crd::echo::{Echo, EchoSpec, EchoStatus};
    use crate::error::Result;

    use std::collections::HashMap;
    use std::sync::Arc;

    use http::{Request, Response};
    use k8s_openapi::api::apps::v1::Deployment;
    use kube::runtime::reflector::store::Writer;
    use kube::{client::Body, Client, Resource, ResourceExt};

    impl Echo {
        /// A normal test echo with a given status
        pub fn test(status: Option<EchoStatus>) -> Self {
            let mut e = Echo::new("test", EchoSpec { replicas: 1 });
            e.meta_mut().namespace = Some("default".into());
            e.status = status;
            e
        }

        /// Modify echo replicas
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
        /// objects changes will cause a patch
        EchoPatch(Echo),
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
                    Scenario::EchoPatch(echo) => self.handle_echo_patch(echo.clone()).await,
                }
                .expect("scenario completed without errors");
            })
        }

        async fn handle_echo_patch(mut self, echo: Echo) -> Result<Self> {
            let (request, send) = self.0.next_request().await.expect("service not called");
            assert_eq!(request.method(), http::Method::PATCH);
            assert_eq!(
                request.uri().to_string(),
                format!(
                    "/apis/apps/v1/namespaces/default/deployments/{}?&force=true&fieldManager=echoes.example.com",
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
    }

    pub fn get_test_context() -> (Arc<Context<Deployment>>, ApiServerVerifier) {
        let (mock_service, handle) = tower_test::mock::pair::<Request<Body>, Response<Body>>();
        let mock_client = Client::new(mock_service, "default");
        let stores = HashMap::from([(
            "deployment".to_string(),
            Box::new(Writer::default().as_reader()),
        )]);
        let ctx = Context {
            client: mock_client,
            metrics: Arc::default(),
            stores: Arc::new(stores),
        };
        (Arc::new(ctx), ApiServerVerifier(handle))
    }
}

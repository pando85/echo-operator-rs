#[cfg(all(test, feature = "integration-tests"))]
mod test {
    use kaniop_operator::crd::echo::{Echo, EchoSpec, EchoStatus};

    #[tokio::test]
    async fn echo_create() {
        let _ = 2;
    }
}

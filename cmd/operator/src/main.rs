use actix_web::{
    get, middleware, web::Data, App, HttpRequest, HttpResponse, HttpServer, Responder,
};
use kaniop_operator::controller::{self, State};
use kaniop_operator::telemetry;

#[get("/metrics")]
async fn metrics(c: Data<State>, _req: HttpRequest) -> impl Responder {
    let metrics = c.metrics();
    HttpResponse::Ok()
        .content_type("application/openmetrics-text; version=1.0.0; charset=utf-8")
        .body(metrics)
}

#[get("/health")]
async fn health(_: HttpRequest) -> impl Responder {
    HttpResponse::Ok().json("healthy")
}

#[get("/")]
async fn index(c: Data<State>, _req: HttpRequest) -> impl Responder {
    let d = c.diagnostics().await;
    HttpResponse::Ok().json(&d)
}

// TODO: Configuration params by CLI and ENV:
//  - port by cli?
//  - logging level
//  - telemetry
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    telemetry::init(false).await;

    let state = State::default();
    let controller = controller::run(state.clone());

    let server = HttpServer::new(move || {
        App::new()
            .app_data(Data::new(state.clone()))
            .wrap(middleware::Logger::default().exclude("/health"))
            .service(index)
            .service(health)
            .service(metrics)
    })
    .bind("0.0.0.0:8080")?
    .shutdown_timeout(5);

    // Both runtimes implements graceful shutdown, so poll until both are done
    tokio::join!(controller, server.run()).1?;
    Ok(())
}

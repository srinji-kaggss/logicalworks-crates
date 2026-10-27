use lgwks_deps::axum::{Json, Router, routing::get};

async fn hello() -> Json<&'static str> {
    Json("hello")
}

fn main() {
    let _app: Router = Router::new().route("/hello", get(hello));
}

use lgwks_deps::tower_http::{compression::CompressionLayer, cors::CorsLayer, trace::TraceLayer};

fn main() {
    let _cors = CorsLayer::permissive();
    let _trace = TraceLayer::new_for_http();
    let _compression = CompressionLayer::new();
}

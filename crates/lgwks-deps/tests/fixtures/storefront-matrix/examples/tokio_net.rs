use lgwks_deps::tokio::net::TcpListener;

async fn bind() -> std::io::Result<TcpListener> {
    TcpListener::bind(("127.0.0.1", 0)).await
}

fn main() {
    let _bind = bind;
}

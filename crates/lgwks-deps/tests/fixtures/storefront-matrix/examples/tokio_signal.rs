use lgwks_deps::tokio::signal;

async fn signal() -> std::io::Result<()> {
    signal::ctrl_c().await
}

fn main() {
    let _signal = signal;
}

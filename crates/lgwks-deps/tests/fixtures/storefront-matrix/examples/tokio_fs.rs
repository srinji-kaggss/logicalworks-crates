use lgwks_deps::tokio::fs::File;

async fn open(path: &std::path::Path) -> std::io::Result<File> {
    File::open(path).await
}

fn main() {
    let _open = open;
}

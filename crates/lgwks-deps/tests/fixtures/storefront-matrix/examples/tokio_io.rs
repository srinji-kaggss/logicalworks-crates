use lgwks_deps::tokio::io::{self, AsyncRead, AsyncReadExt, AsyncWrite};

async fn read(mut input: impl AsyncRead + Unpin) -> std::io::Result<()> {
    let mut byte = [0_u8; 1];
    let _count = input.read(&mut byte).await?;
    Ok(())
}

fn main() {
    let _reader = read(io::empty());
    let _writer = std::any::type_name::<dyn AsyncWrite>();
}

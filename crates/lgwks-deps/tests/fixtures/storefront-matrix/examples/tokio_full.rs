use lgwks_deps::tokio as tokio;
use tokio::io::AsyncRead;
use tokio::net::TcpListener;
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio::time::Duration;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let _duration = Duration::from_millis(1);
    let (_sender, _receiver) = mpsc::channel::<u8>(1);
    let _reader = std::any::type_name::<dyn AsyncRead>();
    let _listener = TcpListener::bind(("127.0.0.1", 0)).await;
    let _command = Command::new("true");
    let _file = tokio::fs::File::open("README.md");
    let _signal = tokio::signal::ctrl_c();
}

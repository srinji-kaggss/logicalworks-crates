//! The split halves of a stream are reachable through `rt::net` alone (#366).
//!
//! moo's MCP proxy writes a request line in one function and reads the reply
//! in another, so it has to *name* the halves `into_split` hands back. These
//! tests are that consumer: every signature below names its types through
//! `lgwks_bot::rt`, never `tokio`, and a real loopback socket carries a line
//! each way, so the names are proven to be the types the stream really splits
//! into rather than ones that merely compile.

#![cfg(all(feature = "net", feature = "io", feature = "time"))]

use std::error::Error;
use std::time::Duration;

use lgwks_bot::Runtime;
use lgwks_bot::rt::io::{AsyncBufReadExt as _, AsyncRead, AsyncWriteExt as _, BufReader};
use lgwks_bot::rt::net::{TcpListener, TcpStream, tcp};
use lgwks_bot::rt::time::timeout;

/// A test's result: an error fails it with the error's text.
type TestResult = Result<(), Box<dyn Error>>;

/// How long one loopback step may take before the test calls it wedged.
const STEP: Duration = Duration::from_secs(10);

/// Read one line from any read half, handing the half back for a reunite.
async fn receive_line<R: AsyncRead + Unpin>(read: R) -> std::io::Result<(String, R)> {
    let mut reader = BufReader::new(read);
    let mut line = String::new();
    reader.read_line(&mut line).await?;
    Ok((line, reader.into_inner()))
}

/// Write one line through an owned TCP write half: the proxy's request side.
async fn send_tcp_line(write: &mut tcp::OwnedWriteHalf, line: &str) -> std::io::Result<()> {
    write.write_all(line.as_bytes()).await?;
    write.write_all(b"\n").await
}

/// Both ends of one loopback TCP stream: the client and the accepted socket.
async fn tcp_pair() -> Result<(TcpStream, TcpStream), Box<dyn Error>> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let (client, accepted) = timeout(STEP, async {
        lgwks_bot::join!(TcpStream::connect(address), listener.accept())
    })
    .await?;
    Ok((client?, accepted?.0))
}

#[test]
fn owned_tcp_halves_carry_a_line_each_way_and_reunite() -> TestResult {
    Runtime::new()?.block_on(async {
        let (client, accepted) = tcp_pair().await?;
        let (client_read, mut client_write): (tcp::OwnedReadHalf, tcp::OwnedWriteHalf) =
            client.into_split();
        let (server_read, mut server_write) = accepted.into_split();

        send_tcp_line(&mut client_write, "request").await?;
        let (request, server_read) = timeout(STEP, receive_line(server_read)).await??;
        assert_eq!(request, "request\n");
        send_tcp_line(&mut server_write, "reply").await?;
        let (reply, client_read) = timeout(STEP, receive_line(client_read)).await??;
        assert_eq!(reply, "reply\n");

        // Halves of two streams are refused with the error the facade names,
        // and each half goes back to the stream it came from.
        let refused: Result<TcpStream, tcp::ReuniteError> = client_read.reunite(server_write);
        let Err(tcp::ReuniteError(client_read, server_write)) = refused else {
            return Err("halves of two streams reunited".into());
        };
        client_read.reunite(client_write)?;
        server_read.reunite(server_write)?;
        Ok(())
    })
}

#[test]
fn borrowed_tcp_halves_are_named_through_the_facade() -> TestResult {
    /// Borrowed halves typed in a signature, as a caller splitting in place
    /// writes it.
    async fn echo_once(
        read: tcp::ReadHalf<'_>,
        mut write: tcp::WriteHalf<'_>,
    ) -> std::io::Result<String> {
        let (line, _) = receive_line(read).await?;
        write.write_all(line.as_bytes()).await?;
        Ok(line)
    }
    Runtime::new()?.block_on(async {
        let (mut client, mut accepted) = tcp_pair().await?;
        client.write_all(b"ping\n").await?;
        let (read, write) = accepted.split();
        assert_eq!(timeout(STEP, echo_once(read, write)).await??, "ping\n");
        let (back, _) = timeout(STEP, receive_line(&mut client)).await??;
        assert_eq!(back, "ping\n");
        Ok(())
    })
}

#[cfg(unix)]
mod unix_socket {
    use std::error::Error;

    use lgwks_bot::Runtime;
    use lgwks_bot::rt::io::AsyncWriteExt as _;
    use lgwks_bot::rt::net::{UnixListener, UnixStream, unix};
    use lgwks_bot::rt::time::timeout;

    use super::{STEP, TestResult, receive_line};
    use crate::scratch::Scratch;

    /// Write one line through an owned Unix write half: moo's request side.
    async fn send_unix_line(write: &mut unix::OwnedWriteHalf, line: &str) -> std::io::Result<()> {
        write.write_all(line.as_bytes()).await?;
        write.write_all(b"\n").await
    }

    /// Both ends of one Unix stream over a socket in `scratch`. The tag and the
    /// socket name are short because a socket path is capped near 104 bytes.
    async fn unix_pair(scratch: &Scratch) -> Result<(UnixStream, UnixStream), Box<dyn Error>> {
        let socket = scratch.path().join("s");
        let listener = UnixListener::bind(&socket)?;
        let (client, accepted) = timeout(STEP, async {
            lgwks_bot::join!(UnixStream::connect(&socket), listener.accept())
        })
        .await?;
        Ok((client?, accepted?.0))
    }

    #[test]
    fn owned_unix_halves_carry_a_line_each_way_and_reunite() -> TestResult {
        let scratch = Scratch::new("ns")?;
        Runtime::new()?.block_on(async {
            let (client, accepted) = unix_pair(&scratch).await?;
            let (client_read, mut client_write) = client.into_split();
            let (server_read, mut server_write): (unix::OwnedReadHalf, unix::OwnedWriteHalf) =
                accepted.into_split();

            send_unix_line(&mut client_write, "request").await?;
            let (request, server_read) = timeout(STEP, receive_line(server_read)).await??;
            assert_eq!(request, "request\n");
            send_unix_line(&mut server_write, "reply").await?;
            let (reply, client_read) = timeout(STEP, receive_line(client_read)).await??;
            assert_eq!(reply, "reply\n");

            let refused: Result<UnixStream, unix::ReuniteError> = client_read.reunite(server_write);
            let Err(unix::ReuniteError(client_read, server_write)) = refused else {
                return Err("halves of two streams reunited".into());
            };
            client_read.reunite(client_write)?;
            server_read.reunite(server_write)?;
            Ok(())
        })
    }

    #[test]
    fn borrowed_unix_halves_are_named_through_the_facade() -> TestResult {
        async fn echo_once(
            read: unix::ReadHalf<'_>,
            mut write: unix::WriteHalf<'_>,
        ) -> std::io::Result<String> {
            let (line, _) = receive_line(read).await?;
            write.write_all(line.as_bytes()).await?;
            Ok(line)
        }
        let scratch = Scratch::new("nb")?;
        Runtime::new()?.block_on(async {
            let (mut client, mut accepted) = unix_pair(&scratch).await?;
            client.write_all(b"ping\n").await?;
            let (read, write) = accepted.split();
            assert_eq!(timeout(STEP, echo_once(read, write)).await??, "ping\n");
            let (back, _) = timeout(STEP, receive_line(&mut client)).await??;
            assert_eq!(back, "ping\n");
            Ok(())
        })
    }
}

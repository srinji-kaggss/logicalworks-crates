//! Async networking: TCP, UDP, and hostname resolution.
//!
//! Sockets reach the network directly here; they do not pass through this
//! crate's HTTP facade. A caller wiring an outbound connection through policy is
//! responsible for applying that policy before handing the address to a socket.

pub use lgwks_deps::tokio::net::{TcpListener, TcpStream, UdpSocket, lookup_host};

/// The owned halves a [`TcpStream::into_split`] hands back, named through
/// the facade so a consumer holding the read half in one function and the
/// write half in another never names `tokio` for the signatures (#366).
///
/// Aliased rather than bare: the TCP and Unix pairs share their type names,
/// and one path per item means the prefix carries which socket they split.
pub use lgwks_deps::tokio::net::tcp::{
    OwnedReadHalf as TcpOwnedReadHalf, OwnedWriteHalf as TcpOwnedWriteHalf,
};

#[cfg(unix)]
pub use lgwks_deps::tokio::net::{UnixDatagram, UnixListener, UnixStream};

/// The owned halves a Unix [`UnixStream::into_split`] hands back (#366).
/// See the TCP pair above for why the names carry their prefix.
///
/// ```no_run
/// use lgwks_bot::rt::net::{UnixOwnedReadHalf, UnixOwnedWriteHalf, UnixStream};
///
/// async fn split_pair(stream: UnixStream) {
///     let (read, write): (UnixOwnedReadHalf, UnixOwnedWriteHalf) = stream.into_split();
///     drop((read, write));
/// }
/// ```
#[cfg(unix)]
pub use lgwks_deps::tokio::net::unix::{
    OwnedReadHalf as UnixOwnedReadHalf, OwnedWriteHalf as UnixOwnedWriteHalf,
};

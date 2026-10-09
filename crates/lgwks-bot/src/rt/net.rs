//! Async networking: TCP, UDP, and hostname resolution.
//!
//! Sockets reach the network directly here; they do not pass through this
//! crate's HTTP facade. A caller wiring an outbound connection through policy is
//! responsible for applying that policy before handing the address to a socket.
//!
//! The split halves live where tokio keeps them, in [`tcp`] and [`unix`], so a
//! path written against tokio's documentation is the same path here under
//! `lgwks_bot::rt` (#366).

pub use lgwks_deps::tokio::net::{TcpListener, TcpStream, UdpSocket, lookup_host};

#[cfg(unix)]
pub use lgwks_deps::tokio::net::{UnixDatagram, UnixListener, UnixStream};

/// The halves a [`TcpStream`] splits into, so a reader and a writer held in
/// separate functions are typed without naming `tokio` (#366).
///
/// [`OwnedReadHalf`](tcp::OwnedReadHalf) and
/// [`OwnedWriteHalf`](tcp::OwnedWriteHalf) come from
/// [`TcpStream::into_split`], and [`ReuniteError`](tcp::ReuniteError) from
/// putting two halves of different streams back together;
/// [`ReadHalf`](tcp::ReadHalf) and [`WriteHalf`](tcp::WriteHalf) borrow the
/// stream through [`TcpStream::split`].
pub mod tcp {
    pub use lgwks_deps::tokio::net::tcp::{
        OwnedReadHalf, OwnedWriteHalf, ReadHalf, ReuniteError, WriteHalf,
    };
}

/// The halves a [`UnixStream`] splits into: the Unix counterpart of [`tcp`],
/// under the same five names (#366).
#[cfg(unix)]
pub mod unix {
    pub use lgwks_deps::tokio::net::unix::{
        OwnedReadHalf, OwnedWriteHalf, ReadHalf, ReuniteError, WriteHalf,
    };
}

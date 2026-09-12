//! Async networking — TCP, UDP, and hostname resolution.
//!
//! Sockets reach the network directly here; they do not pass the estate's HTTP
//! egress gate. A caller wiring an outbound connection through policy is
//! responsible for applying that policy before handing the address to a socket.

pub use lgwks_deps::tokio::net::{TcpListener, TcpStream, UdpSocket, lookup_host};

#[cfg(unix)]
pub use lgwks_deps::tokio::net::{UnixDatagram, UnixListener, UnixStream};

//! Async byte streams: the reader and writer traits, their buffered adapters,
//! and the copy helpers.
//!
//! This is what makes the rest of the async surface composable: [`net`],
//! [`process`], and [`fs`] all produce or consume something that is
//! [`AsyncRead`] or [`AsyncWrite`], and without these traits in scope a consumer
//! can open a socket or a pipe but cannot read from it without naming `tokio`.
//!
//! [`net`]: crate::rt::net
//! [`process`]: crate::rt::process
//! [`fs`]: crate::rt::fs

pub use lgwks_deps::tokio::io::{
    AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncSeek, AsyncSeekExt, AsyncWrite,
    AsyncWriteExt, BufReader, BufStream, BufWriter, DuplexStream, Empty, ReadHalf, Repeat,
    SeekFrom, Sink, Split, Stderr, Stdin, Stdout, Take, WriteHalf, copy, copy_bidirectional,
    duplex, empty, repeat, sink, split,
};

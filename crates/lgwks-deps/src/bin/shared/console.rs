//! The two output streams of a gate binary, locked per line so work running on
//! different threads interleaves whole lines, never halves of them.

use std::io::Write;
use std::sync::Mutex;

/// The two output streams.
pub(crate) struct Console {
    /// Standard output.
    out: Mutex<Box<dyn Write + Send>>,
    /// Standard error.
    err: Mutex<Box<dyn Write + Send>>,
}

impl Console {
    /// A console over the given writers.
    pub(crate) fn new(out: Box<dyn Write + Send>, err: Box<dyn Write + Send>) -> Self {
        Self {
            out: Mutex::new(out),
            err: Mutex::new(err),
        }
    }

    /// Writes a line to standard output.
    pub(crate) fn say(&self, text: &str) {
        Self::line(&self.out, text);
    }

    /// Writes a line to standard error.
    pub(crate) fn warn(&self, text: &str) {
        Self::line(&self.err, text);
    }

    /// Writes a line to one stream.
    fn line(stream: &Mutex<Box<dyn Write + Send>>, text: &str) {
        let mut held = match stream.lock() {
            Ok(held) => held,
            Err(poisoned) => poisoned.into_inner(),
        };
        if let Err(error) = writeln!(held, "{text}").and_then(|()| held.flush()) {
            lgwks_std::trace::debug!(%error, "a console line could not be written");
        }
    }
}

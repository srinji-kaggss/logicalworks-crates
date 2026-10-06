//! The one way the process sims start a shell that leads its own group.
//!
//! Every process family needs a leader whose pid is also its group id, so the
//! group signal under test names exactly the tree the test built. The spawn lives
//! here once so the families cannot drift in how they detach the leader's streams
//! or which group it lands in.

/// Start `sh -c script` as the leader of a new process group, with no streams.
///
/// Its own group, or a group kill would name a group the test does not lead and
/// every member would outlive the test.
pub fn spawn_group_leader(script: &str) -> std::io::Result<std::process::Child> {
    use std::os::unix::process::CommandExt as _;
    std::process::Command::new("sh")
        .arg("-c")
        .arg(script)
        .process_group(0)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
}

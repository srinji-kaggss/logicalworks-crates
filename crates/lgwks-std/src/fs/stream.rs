//! A streaming directory walk: entries out as they are read (#692).
//!
//! Source: POSIX.1-2017 `readdir` names each entry's type in `d_type`, so a
//! walk that needs only the kind pays no `stat` per entry; logical_ci's ADR
//! 0003 records the two `read_dir` loops this replaces (its tier-deps walk
//! needs no stat at all, its write snapshot stats on scoped threads after a
//! `d_type`-only walk).
//!
//! [`walk_dir_iter`] is the same trusted-tree listing as
//! [`super::walk_dir`], with four differences the materializing engine cannot
//! offer: the walk holds no output vector (the caller sees the first entry
//! before the last directory is read); directories are listed from an
//! explicit stack, so depth is bounded by [`WalkOptions::max_depth`] and
//! never by the call stack; an entry's own `lstat` is read only when
//! [`WalkStreamEntry::metadata`] is called, so a kind-only walk stats nothing
//! but the directories it enters; and loop detection is by device and inode
//! when [`WalkOptions::follow_symlinks`] is set, never by canonicalizing
//! every directory. The one `canonicalize` this walk performs is the root's,
//! before the first entry.
//!
//! A skipped subtree is the caller's own filter:
//! [`WalkIter::skip_children`] withholds the most recently yielded directory
//! before the walk enters it, and a withheld directory is never listed, so
//! nothing under it is read. Dropping the iterator ends the walk: no entry
//! is recorded anywhere, so there is nothing to filter twice.
//!
//! The walk is still a trusted-tree listing, as the path walks are: a name
//! replaced mid-walk can redirect what the walk reports, and concurrent
//! replacement is [`super::capability::Dir`]'s threat model, not this one's.

use std::collections::HashSet;
use std::fs::{self, DirEntry, FileType, ReadDir};
use std::io;
use std::path::{Path, PathBuf};
use std::vec;

use super::{FileKind, OmissionStage, WalkFailure, WalkOptions, relative_to_root};

/// What a streaming walk does with one omission, chosen per [`io::ErrorKind`].
///
/// A walk over a live tree meets absence constantly: a directory listed and
/// then removed before it is read is not damage, it is a concurrent build.
/// The policy names that fact per kind instead of once for the whole walk.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum OmissionAction {
    /// Pass over the omission: the entry or directory is absent from the
    /// stream, and the walk continues with the next one.
    Skip,
    /// Yield the omission as an [`Err`] item, and continue the walk after it.
    Report,
}

/// Which omissions a streaming walk passes over and which it reports.
///
/// The default reads `NotFound` as absence — a name the walk listed and
/// cannot now open is gone, not broken — and reports every other kind, so a
/// permission refusal or an I/O failure is evidence rather than a hole.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub struct OmissionPolicy {
    /// What a missing name means.
    pub not_found: OmissionAction,
    /// What an unpermitted read means.
    pub permission_denied: OmissionAction,
    /// What every other failure means.
    pub other: OmissionAction,
}

impl OmissionPolicy {
    /// The policy that answers per kind: absence is skipped, refusals and
    /// failures are reported.
    #[must_use]
    pub const fn new(
        not_found: OmissionAction,
        permission_denied: OmissionAction,
        other: OmissionAction,
    ) -> Self {
        Self {
            not_found,
            permission_denied,
            other,
        }
    }

    /// Decide one error under this policy: `None` passes over it, `Some`
    /// carries the failure the stream yields.
    fn decide(&self, path: PathBuf, stage: OmissionStage, error: io::Error) -> Option<WalkFailure> {
        let action = match error.kind() {
            io::ErrorKind::NotFound => self.not_found,
            io::ErrorKind::PermissionDenied => self.permission_denied,
            _ => self.other,
        };
        match action {
            OmissionAction::Skip => None,
            OmissionAction::Report => Some(WalkFailure::for_stream(path, stage, error)),
        }
    }
}

impl Default for OmissionPolicy {
    /// `NotFound` is absence; everything else is reported.
    fn default() -> Self {
        Self::new(
            OmissionAction::Skip,
            OmissionAction::Report,
            OmissionAction::Report,
        )
    }
}

/// One entry a streaming walk admitted, with the kind `readdir` reported.
///
/// The entry carries no metadata: [`Self::metadata`] reads the entry's own
/// `lstat` on demand, so a walk that needs only kinds never stats. A
/// symlink's kind is the link's, never its target's; whether the walk
/// descends through it is the [`WalkOptions::follow_symlinks`] policy, and
/// the decision costs one resolution per link, not per entry.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct WalkStreamEntry {
    /// Absolute path, rooted at the canonicalized walk root.
    path: PathBuf,
    /// Components of the canonicalized root that lead `path`.
    root_components: usize,
    /// The kind `readdir` reported, without following a final symlink.
    file_type: FileType,
}

impl WalkStreamEntry {
    /// The absolute path, rooted at the canonicalized walk root. An entry
    /// reached through a followed in-root symlink keeps the link's name.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The same path relative to the canonicalized walk root: joining it onto
    /// that root gives [`Self::path`] back. Never empty, because the root
    /// itself is not an entry.
    #[must_use]
    pub fn relative_path(&self) -> &Path {
        relative_to_root(&self.path, self.root_components)
    }

    /// The kind `readdir` reported for this entry, stated from its own type
    /// and never by following it.
    #[must_use]
    pub fn kind(&self) -> FileKind {
        FileKind::of(self.file_type)
    }

    /// The kind bits `readdir` reported, without following a final symlink.
    #[must_use]
    pub fn file_type(&self) -> FileType {
        self.file_type
    }

    /// The entry's own `lstat`: file type, size, permissions and modification
    /// time. This is the one stat the streaming walk performs, and only when
    /// called — a kind-only walk never reaches it.
    ///
    /// # Errors
    ///
    /// The filesystem's own refusal when the entry cannot be stated: a name
    /// removed after it was listed reads as absence here.
    pub fn metadata(&self) -> io::Result<fs::Metadata> {
        fs::symlink_metadata(&self.path)
    }
}

/// Identity of one entered directory, for loop termination.
///
/// A bind mount or a followed symlink can place the same directory under two
/// names; canonical paths grow with depth, while a device and inode pair
/// names the directory itself in sixteen bytes however deep it sits.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
enum DirId {
    /// Unix: the directory's device and inode, from one `lstat`.
    #[cfg(unix)]
    DeviceInode(u64, u64),
    /// Elsewhere: the canonical path of the entered directory.
    #[cfg(not(unix))]
    Canonical(PathBuf),
}

/// Read one listed directory's identity for the visited set.
fn listed_dir_id(entry: &DirEntry) -> io::Result<DirId> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let stat = entry.metadata()?;
        Ok(DirId::DeviceInode(stat.dev(), stat.ino()))
    }
    #[cfg(not(unix))]
    {
        entry.path().canonicalize().map(DirId::Canonical)
    }
}

/// Read one followed symlink target's identity for the visited set.
fn target_dir_id(target: &Path) -> io::Result<DirId> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let stat = fs::symlink_metadata(target)?;
        Ok(DirId::DeviceInode(stat.dev(), stat.ino()))
    }
    #[cfg(not(unix))]
    {
        target.canonicalize().map(DirId::Canonical)
    }
}

/// Read the root's identity for the visited set.
fn root_dir_id(canonical_root: &Path) -> io::Result<DirId> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let stat = fs::symlink_metadata(canonical_root)?;
        Ok(DirId::DeviceInode(stat.dev(), stat.ino()))
    }
    #[cfg(not(unix))]
    {
        Ok(DirId::Canonical(canonical_root.to_path_buf()))
    }
}

/// One directory on the explicit stack, and how its entries are read.
#[derive(Debug)]
struct DirFrame {
    /// Absolute path of the directory being listed.
    absolute: PathBuf,
    /// The same directory relative to the canonicalized walk root.
    relative: PathBuf,
    /// Depth of this directory: the root is depth zero, its children one.
    depth: usize,
    /// Entries not yet yielded: streamed from the listing, or buffered for a
    /// sorted walk.
    reader: FrameReader,
}

/// How one frame's entries reach the caller.
#[derive(Debug)]
enum FrameReader {
    /// Not yet opened: the directory is listed when the walk reaches it.
    Unopened,
    /// Entries yielded as the filesystem returns them.
    Streaming(ReadDir),
    /// Entries buffered for a sorted walk.
    Sorted(vec::IntoIter<DirEntry>),
}

impl FrameReader {
    /// The next listed entry, moved out of the frame.
    fn next_entry(&mut self) -> Option<io::Result<DirEntry>> {
        match *self {
            Self::Unopened => None,
            Self::Streaming(ref mut listing) => listing.next(),
            Self::Sorted(ref mut buffered) => buffered.next().map(Ok),
        }
    }
}

/// What admitting one entry decided.
enum DirDecision {
    /// Yield the entry to the caller.
    Yield(WalkStreamEntry),
    /// Withhold the entry and continue the walk.
    Skip,
    /// Yield the failure to the caller and continue the walk after it.
    Report(WalkFailure),
}

impl DirDecision {
    /// Withhold on `None`, report on `Some`.
    fn from_optional(failure: Option<WalkFailure>) -> Self {
        match failure {
            None => Self::Skip,
            Some(failure) => Self::Report(failure),
        }
    }
}

/// A streaming directory walk: entries out as they are read.
///
/// Built by [`walk_dir_iter`]. The walk holds one open listing per ancestor
/// of the entry being read — never the tree — plus sixteen bytes per entered
/// directory, so its retained state is proportional to depth, not to the
/// tree. Dropping it ends the walk wherever it stands.
#[derive(Debug)]
#[non_exhaustive]
pub struct WalkIter {
    /// The canonicalized root every yielded path is joined onto.
    canonical_root: PathBuf,
    /// Components of `canonical_root`, so an entry can name its relative path.
    root_components: usize,
    /// Depth bound, symlink policy and ordering.
    options: WalkOptions,
    /// Which omissions are passed over and which are reported.
    policy: OmissionPolicy,
    /// Directories entered so far, for loop termination.
    visited: HashSet<DirId>,
    /// The explicit stack: the frame being read is last.
    stack: Vec<DirFrame>,
    /// The absolute path of the frame the walk pushed last, while the walk
    /// has not opened it yet. [`Self::skip_children`] withholds exactly this
    /// frame.
    unopened_frame: Option<PathBuf>,
    /// An omission the policy reported while buffering a sorted directory,
    /// yielded before the buffered entries.
    pending: Option<WalkFailure>,
}

impl WalkIter {
    /// Withhold the most recently yielded directory: it is never listed, so
    /// nothing under it is read.
    ///
    /// Only the directory the last yield pushed, and only while the walk has
    /// not opened it yet, is withheld: the walk opens a directory when it
    /// first reaches for what is below it, and a second yield after that has
    /// already entered it. A withheld directory is not an omission; like the
    /// predicate walks' skip, it is the caller's own policy.
    pub fn skip_children(&mut self) {
        if let Some(unopened) = self.unopened_frame.take()
            && self
                .stack
                .last()
                .is_some_and(|frame| frame.absolute == unopened)
        {
            self.stack.pop();
        }
    }

    /// Open the stack's top frame when it is still unopened.
    ///
    /// Returns the reported omission to yield, if the listing or its
    /// buffering met one the policy reports. A withheld frame never reaches
    /// this: [`Self::skip_children`] removes it while it is still unopened.
    fn open_top(&mut self, top: usize) -> Option<WalkFailure> {
        if !matches!(self.stack[top].reader, FrameReader::Unopened) {
            return None;
        }
        if self
            .unopened_frame
            .as_ref()
            .is_some_and(|unopened| *unopened == self.stack[top].absolute)
        {
            self.unopened_frame = None;
        }
        let absolute = self.stack[top].absolute.clone();
        let relative = self.stack[top].relative.clone();
        let listing = match fs::read_dir(&absolute) {
            Ok(listing) => listing,
            Err(error) => {
                self.stack.pop();
                return self
                    .policy
                    .decide(relative, OmissionStage::ReadEntries, error);
            }
        };
        if !self.options.sort_alphabetically {
            self.stack[top].reader = FrameReader::Streaming(listing);
            return None;
        }
        let mut buffered = Vec::new();
        for item in listing {
            match item {
                Ok(entry) => buffered.push(entry),
                Err(error) => {
                    if let Some(failure) =
                        self.policy
                            .decide(relative.clone(), OmissionStage::ReadEntries, error)
                    {
                        self.pending = Some(failure);
                        break;
                    }
                }
            }
        }
        buffered.sort_by_key(DirEntry::file_name);
        self.stack[top].reader = FrameReader::Sorted(buffered.into_iter());
        None
    }

    /// Record one admitted entry.
    fn record(&self, absolute: PathBuf, file_type: FileType) -> WalkStreamEntry {
        WalkStreamEntry {
            path: absolute,
            root_components: self.root_components,
            file_type,
        }
    }

    /// Push one directory's frame and yield the directory itself.
    fn push_dir(
        &mut self,
        absolute: PathBuf,
        relative: PathBuf,
        depth: usize,
        file_type: FileType,
    ) -> WalkStreamEntry {
        self.stack.push(DirFrame {
            absolute: absolute.clone(),
            relative,
            depth,
            reader: FrameReader::Unopened,
        });
        self.unopened_frame = Some(absolute.clone());
        self.record(absolute, file_type)
    }

    /// Decide one listed directory, or one listed symlink whose target the walk
    /// follows: withhold it past the depth bound, past a loop, or past the
    /// root, or push its frame and yield it.
    ///
    /// Directories and followed symlinks share one decision because they share
    /// one question — is this a new directory the policy admits? — and differ
    /// only in where the identity comes from: the entry's own `lstat`, or the
    /// resolved target's.
    fn admit(
        &mut self,
        entry: &DirEntry,
        absolute: PathBuf,
        relative: PathBuf,
        file_type: FileType,
        parent_depth: usize,
    ) -> DirDecision {
        let depth = parent_depth.saturating_add(1);
        if file_type.is_symlink() {
            let target = match entry.path().canonicalize() {
                Ok(target) => target,
                Err(error) => {
                    return DirDecision::from_optional(self.policy.decide(
                        relative,
                        OmissionStage::SymlinkTarget,
                        error,
                    ));
                }
            };
            if !target.starts_with(&self.canonical_root) {
                return DirDecision::Skip;
            }
            if !(self.options.follow_symlinks && target.is_dir() && depth <= self.options.max_depth)
            {
                return DirDecision::Yield(self.record(absolute, file_type));
            }
            let identity = match target_dir_id(&target) {
                Ok(identity) => identity,
                Err(error) => {
                    return DirDecision::from_optional(self.policy.decide(
                        relative,
                        OmissionStage::SymlinkTarget,
                        error,
                    ));
                }
            };
            return match self.enter(identity, target, relative, depth, file_type) {
                Some(admitted) => DirDecision::Yield(admitted),
                // A revisited target is still a yielded entry, only never
                // descended: the link names a directory the walk reports once.
                None => DirDecision::Yield(self.record(absolute, file_type)),
            };
        }
        if depth > self.options.max_depth {
            return DirDecision::Yield(self.record(absolute, file_type));
        }
        let identity = match listed_dir_id(entry) {
            Ok(identity) => identity,
            Err(error) => {
                return DirDecision::from_optional(self.policy.decide(
                    relative,
                    OmissionStage::EntryType,
                    error,
                ));
            }
        };
        match self.enter(identity, absolute, relative, depth, file_type) {
            Some(admitted) => DirDecision::Yield(admitted),
            None => DirDecision::Skip,
        }
    }

    /// Push one entered directory's frame unless the walk already entered it.
    ///
    /// The loop termination the directory and symlink decisions share: the
    /// first visit descends, every later one does not.
    fn enter(
        &mut self,
        identity: DirId,
        absolute: PathBuf,
        relative: PathBuf,
        depth: usize,
        file_type: FileType,
    ) -> Option<WalkStreamEntry> {
        self.visited
            .insert(identity)
            .then(|| self.push_dir(absolute, relative, depth, file_type))
    }

    /// Offer the next listed entry, descending into directories the policy
    /// admits.
    fn advance(&mut self) -> Option<Result<WalkStreamEntry, WalkFailure>> {
        loop {
            if let Some(failure) = self.pending.take() {
                return Some(Err(failure));
            }
            let top = self.stack.len().checked_sub(1)?;
            if matches!(self.stack[top].reader, FrameReader::Unopened) {
                if let Some(failure) = self.open_top(top) {
                    return Some(Err(failure));
                }
                continue;
            }
            let next = self.stack[top].reader.next_entry();
            match next {
                None => {
                    self.stack.pop();
                }
                Some(Err(error)) => {
                    let relative = self.stack[top].relative.clone();
                    if let Some(failure) =
                        self.policy
                            .decide(relative, OmissionStage::ReadEntries, error)
                    {
                        return Some(Err(failure));
                    }
                }
                Some(Ok(entry)) => {
                    let file_type = match entry.file_type() {
                        Ok(file_type) => file_type,
                        Err(error) => {
                            let relative = self.stack[top].relative.join(entry.file_name());
                            if let Some(failure) =
                                self.policy
                                    .decide(relative, OmissionStage::EntryType, error)
                            {
                                return Some(Err(failure));
                            }
                            continue;
                        }
                    };
                    let relative = self.stack[top].relative.join(entry.file_name());
                    let absolute = self.canonical_root.join(&relative);
                    let depth = self.stack[top].depth;
                    if file_type.is_dir() || file_type.is_symlink() {
                        match self.admit(&entry, absolute, relative, file_type, depth) {
                            DirDecision::Yield(admitted) => return Some(Ok(admitted)),
                            DirDecision::Skip => {}
                            DirDecision::Report(failure) => return Some(Err(failure)),
                        }
                        continue;
                    }
                    return Some(Ok(self.record(absolute, file_type)));
                }
            }
        }
    }
}

impl Iterator for WalkIter {
    /// The next admitted entry, or the next reported omission. A walk with
    /// nothing more to read ends with `None`; a reported omission never ends
    /// it.
    type Item = Result<WalkStreamEntry, WalkFailure>;

    fn next(&mut self) -> Option<Self::Item> {
        self.advance()
    }
}

/// Stream `root`'s entries as they are read, instead of materializing them.
///
/// The same trusted-tree listing as [`super::walk_dir`], in the same walk
/// order, with the same depth and symlink policy from `options`: `max_depth`
/// bounds how deep the walk descends, `follow_symlinks` follows only links
/// whose resolved target stays within the root, and `sort_alphabetically`
/// buffers one directory at a time for deterministic order. An unresolvable
/// root is refused before the first entry, exactly as the strict walk
/// refuses it; every other shortfall is answered by `policy`, per kind.
///
/// # Errors
///
/// An unresolvable root, as [`super::walk_dir`] reports it.
pub fn walk_dir_iter(
    root: impl AsRef<Path>,
    options: &WalkOptions,
    policy: OmissionPolicy,
) -> io::Result<WalkIter> {
    let canonical_root = root.as_ref().canonicalize().map_err(|source| {
        let kind = source.kind();
        io::Error::new(
            kind,
            WalkFailure::for_stream(
                root.as_ref().to_path_buf(),
                OmissionStage::RootResolution,
                source,
            ),
        )
    })?;
    let root_components = canonical_root.components().count();
    let mut visited = HashSet::new();
    if let Ok(identity) = root_dir_id(&canonical_root) {
        visited.insert(identity);
    }
    let mut walk = WalkIter {
        canonical_root: canonical_root.clone(),
        root_components,
        options: options.clone(),
        policy,
        visited,
        stack: Vec::new(),
        unopened_frame: None,
        pending: None,
    };
    walk.stack.push(DirFrame {
        absolute: canonical_root,
        relative: PathBuf::new(),
        depth: 0,
        reader: FrameReader::Unopened,
    });
    Ok(walk)
}

#[cfg(test)]
mod tests;

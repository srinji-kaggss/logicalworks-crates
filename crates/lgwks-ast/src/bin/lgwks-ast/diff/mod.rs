//! `lgwks-ast script diff <base> <head>`: the architecture change between two
//! source roots (#655).
//!
//! Every `script!` block emits an `ARCHITECTURE` map: its flows and, per step,
//! its word, subject and line. `script map` and `script check` read that map
//! from source without compiling, and this verb reads two maps and compares
//! them.
//!
//! There is no git call here. `<base>` and `<head>` are two source roots the
//! caller checked out — two directories, or two files — and the verb reads both
//! from disk. A CI step checks a revision out beside the base revision and asks
//! for the diff between the two directories, so the command holds no
//! repository, branch or commit knowledge of its own.
//!
//! The comparison is structural: flow name and signature, then each step's
//! word, subject and nesting path. `line` is never compared, so inserting a
//! line above a flow is an empty diff. The one marker that matters is
//! `// lgwks-architecture-approved: <reason>` in the head source: it approves
//! every widening in the file it is written in. A source marker is chosen over
//! an `--allow-widening` flag because a flag is one word in a CI recipe that can
//! be added without a reviewer seeing a reason, while the marker is a reviewed
//! line that must carry one.

mod classify;
mod render;

use std::collections::{BTreeMap, BTreeSet};
use std::io::{self, Write};
use std::path::Path;
use std::process::ExitCode;

use lgwks_ast::script::{FlowShape, Kind, StepShape, read_source};

use super::{USAGE, files_under, read_bounded};

/// The comment an author adds to the head source to approve a widening.
const APPROVAL_PREFIX: &str = "// lgwks-architecture-approved:";

/// The exit status of an architecture that widens without approval.
const EXIT_WIDENING: u8 = 1;

/// The exit status of a usage or parse error, matching the rest of the tool.
const EXIT_REFUSED: u8 = 2;

/// How a change moves the architecture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Class {
    /// More work, a wider domain, a looser bound or a removed deadline.
    Widening,
    /// The reverse of a widening.
    Narrowing,
    /// A change with no direction, or one the source does not let us read.
    Neutral,
}

impl Class {
    /// The word the JSON and the summary line use.
    const fn word(self) -> &'static str {
        match self {
            Self::Widening => "widening",
            Self::Narrowing => "narrowing",
            Self::Neutral => "neutral",
        }
    }

    /// The heading the Markdown section uses.
    const fn title(self) -> &'static str {
        match self {
            Self::Widening => "Widening",
            Self::Narrowing => "Narrowing",
            Self::Neutral => "Neutral",
        }
    }
}

/// One structural change between the two revisions.
struct Change {
    /// The direction the change moves the architecture.
    class: Class,
    /// The file the change is in, relative to its root.
    file: String,
    /// The flow the change is in; empty for a flow-level change.
    flow: String,
    /// The nesting path from the flow down to the changed step.
    path: String,
    /// The step's word, as the JSON map spells it; `Flow` for a signature.
    kind: String,
    /// The value before the change; empty for an addition.
    from: String,
    /// The value after the change; empty for a removal.
    to: String,
    /// Why the change was classified this way.
    reason: String,
    /// Whether the head source carries an approval marker for its file.
    approved: bool,
}

/// Everything the two roots changed, and the approvals the head declares.
struct Diff {
    /// Every change, in report order.
    changes: Vec<Change>,
    /// Each head file's approval reason, by file.
    approvals: BTreeMap<String, String>,
}

impl Diff {
    /// How many widening changes no marker approves.
    fn unapproved_widening(&self) -> usize {
        self.changes
            .iter()
            .filter(|change| change.class == Class::Widening && !change.approved)
            .count()
    }

    /// How many changes have `class`.
    fn count(&self, class: Class) -> usize {
        self.changes
            .iter()
            .filter(|change| change.class == class)
            .count()
    }

    /// True when no flow, word, subject or nesting path differs.
    fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }
}

/// One revision's scripts, keyed by the file they were read from.
struct Root {
    /// Every file read, in relative-path order.
    files: BTreeMap<String, FileMap>,
}

/// One file's map and its approval markers.
struct FileMap {
    /// The reason each marker line gives, in source order.
    approvals: Vec<String>,
    /// The flows the file's `script!` blocks declare, in source order.
    flows: Vec<FlowShape>,
}

/// Where the report is about, carried through the comparison.
struct Context<'a> {
    /// The file, relative to its root.
    file: &'a str,
    /// The flow the change is in.
    flow: &'a str,
    /// Whether the head file carries an approval marker.
    approved: bool,
}

/// A step's structural identity among its siblings: its word, the name that
/// identifies it when the word carries one, and its ordinal among the siblings
/// with that pair.
type Key = (String, String, usize);

/// Run `script diff <base> <head>`.
///
/// # Errors
///
/// The writer's own error. A usage or parse refusal is a message on `err` and
/// [`EXIT_REFUSED`], not an I/O error.
pub(super) fn run(
    base: &Path,
    head: &Path,
    json: bool,
    out: &mut impl Write,
    err: &mut impl Write,
) -> io::Result<ExitCode> {
    let base_directory = match is_directory(base) {
        Ok(is_directory) => is_directory,
        Err(message) => return fail(err, &message),
    };
    let head_directory = match is_directory(head) {
        Ok(is_directory) => is_directory,
        Err(message) => return fail(err, &message),
    };
    if base_directory != head_directory {
        return usage(
            err,
            "`script diff` takes two directories or two files, not one of each",
        );
    }
    let base_root = match read_root(base) {
        Ok(root) => root,
        Err(message) => return fail(err, &message),
    };
    let head_root = match read_root(head) {
        Ok(root) => root,
        Err(message) => return fail(err, &message),
    };
    let diff = compare(&base_root, &head_root);
    if json {
        render::write_json(&diff, base, head, out)?;
    } else {
        render::write_markdown(&diff, base, head, out)?;
    }
    let status = if diff.unapproved_widening() == 0 {
        0
    } else {
        EXIT_WIDENING
    };
    Ok(ExitCode::from(status))
}

/// Print a refusal and answer the tool's parse refusal, without the usage:
/// `script check` prints a path it cannot read the same way.
fn fail(err: &mut impl Write, message: &str) -> io::Result<ExitCode> {
    writeln!(err, "lgwks-ast: {message}")?;
    Ok(ExitCode::from(EXIT_REFUSED))
}

/// Print a usage refusal and the usage.
fn usage(err: &mut impl Write, message: &str) -> io::Result<ExitCode> {
    writeln!(err, "lgwks-ast: {message}\n")?;
    write!(err, "{USAGE}")?;
    Ok(ExitCode::from(EXIT_REFUSED))
}

/// Whether `root` is a directory; a path that cannot be stated is refused by
/// name, as the walk refuses it.
fn is_directory(root: &Path) -> Result<bool, String> {
    std::fs::metadata(root)
        .map(|metadata| metadata.is_dir())
        .map_err(|error| format!("cannot read {}: {error}", root.display()))
}

/// Read every script under `root`, keyed by its path relative to `root`.
///
/// # Errors
///
/// A root that cannot be walked, a file that cannot be read, or a `script!`
/// the language refuses: a check that skipped one would compare a tree it did
/// not read.
fn read_root(root: &Path) -> Result<Root, String> {
    let mut files = BTreeMap::new();
    for file in files_under(root)? {
        let text = read_bounded(&file)?;
        let invocations = read_source(&text).map_err(|refusal| located(&file, &refusal))?;
        let mut entry = FileMap {
            approvals: approvals(&text),
            flows: Vec::new(),
        };
        for invocation in &invocations {
            match invocation.read() {
                Ok(script) => entry
                    .flows
                    .extend(script.flows().iter().map(|flow| flow.shape().clone())),
                Err(refusal) => return Err(refused(&file, refusal)),
            }
        }
        files.insert(relative(&file, root), entry);
    }
    Ok(Root { files })
}

/// Where a refusal points, as `path:line:column` with a 1-based column.
fn position(path: &Path, refusal: &lgwks_ast::script::Refusal) -> String {
    format!(
        "{}:{}:{}",
        path.display(),
        refusal.line(),
        refusal.column().saturating_add(1)
    )
}

/// A file the lexer could not read, located where it stopped.
fn located(path: &Path, refusal: &lgwks_ast::script::Refusal) -> String {
    format!("{}: {}", position(path, refusal), refusal.message())
}

/// A `script!` the language refused, prefixed as `script check` prefixes it.
fn refused(path: &Path, refusal: &lgwks_ast::script::Refusal) -> String {
    format!(
        "{}: refused: {}",
        position(path, refusal),
        refusal.message()
    )
}

/// `file` relative to `root`, without a leading `./`.
fn relative(file: &Path, root: &Path) -> String {
    let stripped = match file.strip_prefix(root) {
        Ok(relative) => relative,
        Err(_not_below_root) => file,
    };
    let shown = stripped.display().to_string();
    match shown.strip_prefix("./") {
        Some(rest) => rest.to_owned(),
        None => shown,
    }
}

/// The reasons the approval markers in `text` give, one per marker.
fn approvals(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|line| line.trim().strip_prefix(APPROVAL_PREFIX))
        .map(str::trim)
        .filter(|reason| !reason.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Compare two revisions' maps.
fn compare(base: &Root, head: &Root) -> Diff {
    let mut changes = Vec::new();
    let mut approvals = BTreeMap::new();
    for (file, base_entry) in &base.files {
        let head_entry = head.files.get(file);
        let approved = head_entry.is_some_and(|entry| !entry.approvals.is_empty());
        if let Some(reason) = head_entry.and_then(|entry| entry.approvals.first()) {
            approvals.insert(file.clone(), reason.clone());
        }
        let context = Context {
            file,
            flow: "",
            approved,
        };
        compare_flows(&context, base_entry, head_entry, &mut changes);
    }
    for (file, head_entry) in &head.files {
        if base.files.contains_key(file) {
            continue;
        }
        if let Some(reason) = head_entry.approvals.first() {
            approvals.insert(file.clone(), reason.clone());
        }
        let approved = !head_entry.approvals.is_empty();
        for flow in &head_entry.flows {
            let inner = Context {
                file,
                flow: flow.name(),
                approved,
            };
            note(
                &mut changes,
                &inner,
                flow.name().to_owned(),
                "Flow".to_owned(),
                String::new(),
                flow.signature().to_owned(),
                classify::flow_added(),
            );
        }
    }
    Diff { changes, approvals }
}

/// Compare the flows of one file, matching them by name in source order.
fn compare_flows(
    context: &Context<'_>,
    base: &FileMap,
    head: Option<&FileMap>,
    changes: &mut Vec<Change>,
) {
    let mut used = match head {
        Some(entry) => vec![false; entry.flows.len()],
        None => Vec::new(),
    };
    for base_flow in &base.flows {
        let matched = head.and_then(|entry| {
            entry
                .flows
                .iter()
                .enumerate()
                .position(|(index, flow)| {
                    used.get(index).copied() != Some(true) && flow.name() == base_flow.name()
                })
                .map(|index| (index, &entry.flows[index]))
        });
        match matched {
            Some((index, head_flow)) => {
                if let Some(slot) = used.get_mut(index) {
                    *slot = true;
                }
                let inner = Context {
                    file: context.file,
                    flow: head_flow.name(),
                    approved: context.approved,
                };
                compare_flow(&inner, base_flow, head_flow, changes);
            }
            None => {
                let inner = Context {
                    file: context.file,
                    flow: base_flow.name(),
                    approved: context.approved,
                };
                note(
                    changes,
                    &inner,
                    base_flow.name().to_owned(),
                    "Flow".to_owned(),
                    base_flow.signature().to_owned(),
                    String::new(),
                    classify::flow_removed(),
                );
            }
        }
    }
    let Some(entry) = head else {
        return;
    };
    for (index, head_flow) in entry.flows.iter().enumerate() {
        if used.get(index).copied() == Some(true) {
            continue;
        }
        let inner = Context {
            file: context.file,
            flow: head_flow.name(),
            approved: context.approved,
        };
        note(
            changes,
            &inner,
            head_flow.name().to_owned(),
            "Flow".to_owned(),
            String::new(),
            head_flow.signature().to_owned(),
            classify::flow_added(),
        );
    }
}

/// Compare one matched flow: its signature, then its steps.
fn compare_flow(
    context: &Context<'_>,
    base: &FlowShape,
    head: &FlowShape,
    changes: &mut Vec<Change>,
) {
    if base.signature() != head.signature() {
        note(
            changes,
            context,
            head.name().to_owned(),
            "Flow".to_owned(),
            base.signature().to_owned(),
            head.signature().to_owned(),
            (
                Class::Neutral,
                "the flow's signature changed: its inputs or output differ".to_owned(),
            ),
        );
    }
    compare_steps(context, "", base.steps(), head.steps(), changes);
}

/// One level of the tree: match siblings by structural identity, compare the
/// matched pairs, and report the rest as added or removed.
///
/// A removed block promotes its children into this level, and an added block
/// absorbs a removed sibling into itself. A child that has no sibling of its
/// own key and does have a counterpart on the other side is that same step
/// moved, not a new one: it is compared here, under its new path, and the
/// block's own addition or removal carries the classification. A step still
/// inside an added block, or still lost with a removed one, is reported with
/// that block so a widening nested there cannot hide.
fn compare_steps(
    context: &Context<'_>,
    parent: &str,
    base: &[StepShape],
    head: &[StepShape],
    changes: &mut Vec<Change>,
) {
    let (base_order, base_by_key) = index_steps(base);
    let (head_order, head_by_key) = index_steps(head);
    let promoted = promoted_out(&base_order, &base_by_key, &head_by_key);
    let absorbed = absorbed_in(&head_order, &base_by_key, &head_by_key);
    for (key, head_step) in head_order {
        let path = join(parent, &segment(head_step));
        if let Some(base_step) = promoted.get(&key) {
            compare_step(context, &path, base_step, head_step, changes);
            continue;
        }
        let matched = base_by_key
            .get(&key)
            .and_then(|index| base_order.get(*index))
            .map(|&(_, step)| step);
        match matched {
            Some(base_step) => compare_step(context, &path, base_step, head_step, changes),
            None => {
                note(
                    changes,
                    context,
                    path.clone(),
                    format!("{:?}", head_step.kind()),
                    String::new(),
                    display_of(head_step),
                    classify::added(head_step.kind()),
                );
                added_subtree(
                    context,
                    &path,
                    head_step,
                    &absorbed,
                    &base_order,
                    &base_by_key,
                    changes,
                );
            }
        }
    }
    for (key, base_step) in base_order {
        if head_by_key.contains_key(&key) || absorbed.contains(&key) {
            continue;
        }
        let path = join(parent, &segment(base_step));
        note(
            changes,
            context,
            path.clone(),
            format!("{:?}", base_step.kind()),
            display_of(base_step),
            String::new(),
            classify::removed(base_step.kind()),
        );
        removed_subtree(context, &path, base_step, &promoted, changes);
    }
}

/// The children a removed block promotes into this level, keyed by the key of
/// the added sibling they match. A child with a sibling of its own key stays
/// where it was, and a child with no added counterpart is gone with its block.
fn promoted_out<'a>(
    base_order: &[(Key, &'a StepShape)],
    base_by_key: &BTreeMap<Key, usize>,
    head_by_key: &BTreeMap<Key, usize>,
) -> BTreeMap<Key, &'a StepShape> {
    let mut promoted = BTreeMap::new();
    for &(ref key, base_step) in base_order {
        if head_by_key.contains_key(key) || base_step.children().is_empty() {
            continue;
        }
        for (child_key, child) in index_steps(base_step.children()).0 {
            if !base_by_key.contains_key(&child_key)
                && head_by_key.contains_key(&child_key)
                && !promoted.contains_key(&child_key)
            {
                promoted.insert(child_key, child);
            }
        }
    }
    promoted
}

/// The removed siblings an added block absorbs, by the key of the child that
/// absorbs them. A child whose key a sibling already holds is a second step of
/// that name, not the removed one.
fn absorbed_in(
    head_order: &[(Key, &StepShape)],
    base_by_key: &BTreeMap<Key, usize>,
    head_by_key: &BTreeMap<Key, usize>,
) -> BTreeSet<Key> {
    let mut absorbed = BTreeSet::new();
    for &(ref key, head_step) in head_order {
        if base_by_key.contains_key(key) || head_step.children().is_empty() {
            continue;
        }
        for (child_key, _child) in index_steps(head_step.children()).0 {
            if base_by_key.contains_key(&child_key)
                && !head_by_key.contains_key(&child_key)
                && !absorbed.contains(&child_key)
            {
                absorbed.insert(child_key);
            }
        }
    }
    absorbed
}

/// Report the steps still inside an added block, comparing the one an added
/// sibling absorbed and reporting the rest as added, block by block.
fn added_subtree(
    context: &Context<'_>,
    parent: &str,
    head: &StepShape,
    absorbed: &BTreeSet<Key>,
    base_order: &[(Key, &StepShape)],
    base_by_key: &BTreeMap<Key, usize>,
    changes: &mut Vec<Change>,
) {
    for (key, child) in index_steps(head.children()).0 {
        let path = join(parent, &segment(child));
        if absorbed.contains(&key) {
            let matched = base_by_key
                .get(&key)
                .and_then(|index| base_order.get(*index))
                .map(|&(_, step)| step);
            if let Some(base_step) = matched {
                compare_step(context, &path, base_step, child, changes);
            }
            continue;
        }
        note(
            changes,
            context,
            path.clone(),
            format!("{:?}", child.kind()),
            String::new(),
            display_of(child),
            classify::added(child.kind()),
        );
        added_subtree(
            context,
            &path,
            child,
            absorbed,
            base_order,
            base_by_key,
            changes,
        );
    }
}

/// Report the steps still inside a removed block, except the ones an added
/// sibling promoted out of it, block by block.
fn removed_subtree(
    context: &Context<'_>,
    parent: &str,
    base: &StepShape,
    promoted: &BTreeMap<Key, &StepShape>,
    changes: &mut Vec<Change>,
) {
    for (key, child) in index_steps(base.children()).0 {
        if promoted.contains_key(&key) {
            continue;
        }
        let path = join(parent, &segment(child));
        note(
            changes,
            context,
            path.clone(),
            format!("{:?}", child.kind()),
            display_of(child),
            String::new(),
            classify::removed(child.kind()),
        );
        removed_subtree(context, &path, child, promoted, changes);
    }
}

/// Compare one matched pair: its subject, then the steps inside it.
fn compare_step(
    context: &Context<'_>,
    path: &str,
    base: &StepShape,
    head: &StepShape,
    changes: &mut Vec<Change>,
) {
    if base.subject() != head.subject() {
        note(
            changes,
            context,
            path.to_owned(),
            format!("{:?}", head.kind()),
            base.subject().to_owned(),
            head.subject().to_owned(),
            classify::subject(base.kind(), base.subject(), head.subject()),
        );
    } else if base.detail() != head.detail() {
        note(
            changes,
            context,
            path.to_owned(),
            format!("{:?}", head.kind()),
            base.detail().to_owned(),
            head.detail().to_owned(),
            classify::detail(head.kind(), base.detail(), head.detail()),
        );
    }
    compare_steps(context, path, base.children(), head.children(), changes);
}

/// Index `steps` in source order, and by structural identity.
fn index_steps(steps: &[StepShape]) -> (Vec<(Key, &StepShape)>, BTreeMap<Key, usize>) {
    let mut counts: BTreeMap<(String, String), usize> = BTreeMap::new();
    let mut order = Vec::with_capacity(steps.len());
    let mut lookup = BTreeMap::new();
    for step in steps {
        let pair = (format!("{:?}", step.kind()), identity(step).to_owned());
        let ordinal = match counts.get(&pair) {
            Some(count) => *count,
            None => 0,
        };
        counts.insert(pair.clone(), ordinal.saturating_add(1));
        let key = (pair.0, pair.1, ordinal);
        lookup.insert(key.clone(), order.len());
        order.push((key, step));
    }
    (order, lookup)
}

/// The name that identifies a step when its word carries one: a `step`'s name
/// or a `run`'s callee. Every other word is identified by its ordinal alone, so
/// a changed bound is one matched pair rather than an addition and a removal.
fn identity(step: &StepShape) -> &str {
    match step.kind() {
        Kind::Step | Kind::Run => step.subject(),
        _ => "",
    }
}

/// A step's segment of the nesting path: its word and, when the word carries a
/// bound, name or call, its subject.
fn segment(step: &StepShape) -> String {
    let word = step.kind().spelling();
    match step.kind() {
        Kind::Step | Kind::Run | Kind::Within | Kind::Retry | Kind::Each => {
            format!("{word} {}", step.subject())
        }
        _ => word.to_owned(),
    }
}

/// The value a person reads for a step: its subject, or the line when the word
/// has no subject.
fn display_of(step: &StepShape) -> String {
    if step.subject().is_empty() {
        step.detail().to_owned()
    } else {
        step.subject().to_owned()
    }
}

/// `parent` extended by one segment, or the segment alone at the flow level.
fn join(parent: &str, segment: &str) -> String {
    if parent.is_empty() {
        segment.to_owned()
    } else {
        format!("{parent} > {segment}")
    }
}

/// Record one change.
fn note(
    changes: &mut Vec<Change>,
    context: &Context<'_>,
    path: String,
    kind: String,
    from: String,
    to: String,
    verdict: (Class, String),
) {
    changes.push(Change {
        class: verdict.0,
        file: context.file.to_owned(),
        flow: context.flow.to_owned(),
        path,
        kind,
        from,
        to,
        reason: verdict.1,
        approved: context.approved,
    });
}

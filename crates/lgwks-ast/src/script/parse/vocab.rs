//! The structured words: `graph` and its `node`s, `saga` and its
//! `compensate` lines, `whenever`, `justify`, `remember`, `admit` and `ready`.

use std::collections::{BTreeMap, BTreeSet};

use lgwks_deps::proc_macro2::{TokenStream, TokenTree};

use super::super::lexicon::{self, Kind};
use super::super::lines::{
    Line, Node as LineNode, after, group, ident, is_ident, is_parens, is_punct, text,
};
use super::super::refuse;
use super::super::tree::{
    Admit, Block, Compensate, Construct, Graph, Justify, Node, Overflow, Producer, Ready, Site,
    Statement, Step, StepShape, Whenever,
};
use super::super::{Refusal, Result};
use super::words::{bound, duration, refuse_typed_concurrency, rewrite, shape};
use super::{Labels, Place, block};

/// The largest buffer `whenever` takes: the bound `each` takes, for the same
/// reason — past it the number is a memory budget, not a burst.
const MAX_BUFFER: u64 = 65_536;

/// Refuse at `line` with `message`, logged as every refusal is.
fn refuse_at<T>(line: &Line, message: impl Into<String>, at: &str) -> Result<T> {
    let refusal = Err(Refusal::new(line.span, message));
    tracing::debug!(error = ?refusal.as_ref().err(), word = at, "refusing a structured word");
    refusal
}

/// The body of a structured word: a fresh scope for labels, `give back`
/// refused, the last line as the value, and `compensate` allowed only when
/// `saga` says this is a saga's own body.
fn word_body(children: &[LineNode], saga: bool) -> Result<(Block, Vec<StepShape>)> {
    let mut labels = Labels::default();
    block(
        children,
        &mut labels,
        Place {
            nested: true,
            wants_value: true,
            saga,
        },
    )
}

/// `graph[, at most (limit) at once]:` and its `node`s.
pub(super) fn graph(
    line: &Line,
    children: &[LineNode],
    labels: &mut Labels,
) -> Result<(Construct, StepShape)> {
    let (written_bound, subject) = match *after(&line.tokens, 1) {
        [] => (None, String::from("sized to the machine")),
        [
            ref comma,
            ref at,
            ref most,
            ref limit @ ..,
            ref at_word,
            ref once,
        ] if is_punct(comma, ',')
            && is_ident(at, "at")
            && is_ident(most, "most")
            && is_ident(at_word, "at")
            && is_ident(once, "once")
            && !limit.is_empty() =>
        {
            refuse_typed_concurrency(limit)?;
            (
                Some(bound(limit, line, "at most", MAX_BUFFER)?),
                text(limit),
            )
        }
        _ => {
            return refuse_at(
                line,
                "`graph` reads `graph:` or `graph, at most (limit) at once:`",
                "graph",
            );
        }
    };
    if children.is_empty() {
        return refuse_at(
            line,
            "a `graph` runs at least one `node name:` written under it",
            "graph",
        );
    }
    let mut nodes = Vec::with_capacity(children.len());
    let mut shapes = Vec::with_capacity(children.len());
    for child in children {
        let (node, map) = node(child)?;
        nodes.push(node);
        shapes.push(map);
    }
    refuse_unrunnable(line, &nodes)?;
    let label = labels.next("graph");
    let map = shape(Kind::Graph, subject, text(&line.tokens), line, shapes);
    let declared = Graph {
        site: Site::of(&map),
        label,
        bound: written_bound,
        nodes,
    };
    Ok((Construct::Graph(declared), map))
}

/// One `node name[ needs a, b]:` under a `graph`.
fn node(child: &LineNode) -> Result<(Node, StepShape)> {
    let line = &child.line;
    refuse::check(&line.tokens)?;
    let form = "a line under `graph:` reads `node name:` or `node name needs a, b:`";
    if lexicon::kind_of(line) != Some(Kind::Node) || !line.opens_block {
        return refuse_at(line, form, "node");
    }
    let rest = after(&line.tokens, 1);
    let Some((name, rest)) = rest
        .split_first()
        .and_then(|(first, tail)| ident(first).map(|name| (name.to_string(), tail)))
    else {
        return refuse_at(line, form, "node");
    };
    let needs = match *rest {
        [] => Vec::new(),
        [ref word, ref listed @ ..] if is_ident(word, "needs") && !listed.is_empty() => {
            names(line, listed, form)?
        }
        _ => return refuse_at(line, form, "node"),
    };
    let (body, inner) = word_body(&child.children, false)?;
    let map = shape(Kind::Node, name.clone(), text(&line.tokens), line, inner);
    let declared = Node {
        site: Site::of(&map),
        name,
        needs,
        body,
    };
    Ok((declared, map))
}

/// `a, b, c`: names separated by commas, each one word.
fn names(line: &Line, listed: &[TokenTree], form: &str) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for written in listed.split(|token| is_punct(token, ',')) {
        match *written {
            [ref only] if let Some(name) = ident(only) => out.push(name.to_string()),
            _ => return refuse_at(line, form, "node"),
        }
    }
    Ok(out)
}

/// Refuse a graph that cannot run: a name declared twice, a need no node
/// declares, a node needing itself, or a cycle, each named.
fn refuse_unrunnable(line: &Line, nodes: &[Node]) -> Result<()> {
    let mut declared: BTreeSet<&str> = BTreeSet::new();
    for node in nodes {
        if !declared.insert(node.name.as_str()) {
            return refuse_at(
                line,
                format!(
                    "the node `{}` is declared twice; each node has its own name",
                    node.name
                ),
                "graph",
            );
        }
    }
    for node in nodes {
        for need in &node.needs {
            if *need == node.name {
                return refuse_at(
                    line,
                    format!("the node `{need}` needs itself, so it could never start"),
                    "graph",
                );
            }
            if !declared.contains(need.as_str()) {
                return refuse_at(
                    line,
                    format!(
                        "the node `{}` needs `{need}`, which no `node` line declares",
                        node.name
                    ),
                    "graph",
                );
            }
        }
    }
    // Kahn's pass: what never reaches zero waits on a cycle.
    let mut waiting: BTreeMap<&str, usize> = nodes
        .iter()
        .map(|node| (node.name.as_str(), node.needs.len()))
        .collect();
    let mut ready: Vec<&str> = waiting
        .iter()
        .filter(|entry| *entry.1 == 0)
        .map(|entry| *entry.0)
        .collect();
    while let Some(done) = ready.pop() {
        waiting.remove(done);
        for node in nodes {
            if node.needs.iter().any(|need| need == done)
                && let Some(count) = waiting.get_mut(node.name.as_str())
            {
                *count = count.saturating_sub(1);
                if *count == 0 {
                    ready.push(node.name.as_str());
                }
            }
        }
    }
    match waiting.keys().next() {
        None => Ok(()),
        Some(stuck) => refuse_at(
            line,
            format!("the needs form a cycle through `{stuck}`, so no node of it could ever start"),
            "graph",
        ),
    }
}

/// `saga name:`, a named scope whose body may `compensate` its steps, or
/// `remember name:`, a named scope whose value is recorded once per run.
///
/// One reader for both, because both are a one-word name over a body; a
/// `saga`'s own body is the one place `compensate` stands.
pub(super) fn named(
    line: &Line,
    children: &[LineNode],
    labels: &mut Labels,
    kind: Kind,
) -> Result<(Construct, StepShape)> {
    let word = kind.spelling();
    let name = match *after(&line.tokens, 1) {
        [ref only] => ident(only),
        _ => None,
    };
    let Some(name) = name else {
        return refuse_at(
            line,
            format!("`{word}` reads `{word} <name>:`, where the name is one word"),
            word,
        );
    };
    let name = name.to_string();
    let label = labels.next(&name);
    let (body, inner) = word_body(children, kind == Kind::Saga)?;
    let map = shape(kind, name, text(&line.tokens), line, inner);
    let named = Step {
        site: Site::of(&map),
        label,
        body,
    };
    let construct = if kind == Kind::Saga {
        Construct::Saga(named)
    } else {
        Construct::Remember(named)
    };
    Ok((construct, map))
}

/// `compensate step:` in a saga's own body, after the `step` it undoes.
pub(super) fn compensate(
    node: &LineNode,
    place: Place,
    earlier: &[Statement],
) -> Result<(Statement, StepShape)> {
    let line = &node.line;
    refuse::check(&line.tokens)?;
    if !place.saga {
        return refuse_at(
            line,
            "`compensate` undoes a step of a saga: write it directly in a `saga name:` body, after that step",
            "compensate",
        );
    }
    let undoes = match *after(&line.tokens, 1) {
        [ref only] if line.opens_block => ident(only).map(ToString::to_string),
        _ => None,
    };
    let Some(undoes) = undoes else {
        return refuse_at(
            line,
            "`compensate` reads `compensate <step>:`, where the step is one word",
            "compensate",
        );
    };
    let steps: Vec<&str> = earlier
        .iter()
        .filter_map(|statement| match *statement {
            Statement::Construct(Construct::Step(ref step))
            | Statement::Bind {
                block: Construct::Step(ref step),
                ..
            } => Some(step.label.as_str()),
            _ => None,
        })
        .collect();
    if !steps.contains(&undoes.as_str()) {
        let written = if steps.is_empty() {
            String::from("none yet")
        } else {
            steps
                .iter()
                .map(|step| format!("`{step}`"))
                .collect::<Vec<_>>()
                .join(", ")
        };
        return refuse_at(
            line,
            format!(
                "`compensate {undoes}` undoes a step written before it in this saga; the steps so far: {written}"
            ),
            "compensate",
        );
    }
    let (undo, inner) = word_body(&node.children, false)?;
    let map = shape(
        Kind::Compensate,
        undoes.clone(),
        text(&line.tokens),
        line,
        inner,
    );
    let registered = Compensate {
        site: Site::of(&map),
        undoes,
        undo,
    };
    Ok((Statement::Compensate(registered), map))
}

/// `whenever <pattern> in <batches>, buffering <N>, <overflow>:`.
pub(super) fn whenever(
    line: &Line,
    children: &[LineNode],
    labels: &mut Labels,
) -> Result<(Construct, StepShape)> {
    let form = "`whenever` reads `whenever <event> in <batches>, buffering <N>, drop oldest:` \
                (or `drop newest`, `refuse`)";
    let tokens = after(&line.tokens, 1);
    let Some(at_in) = tokens.iter().position(|token| is_ident(token, "in")) else {
        return refuse_at(line, form, "whenever");
    };
    let (pattern, at_keyword) = tokens.split_at(at_in);
    let rest = after(at_keyword, 1);
    let Some(at_buffering) = rest.windows(2).position(|pair| {
        matches!(*pair, [ref comma, ref word] if is_punct(comma, ',') && is_ident(word, "buffering"))
    }) else {
        return refuse_at(line, form, "whenever");
    };
    let (batches, clause) = rest.split_at(at_buffering);
    let clause = after(clause, 2);
    let Some(at_policy) = clause.iter().position(|token| is_punct(token, ',')) else {
        return refuse_at(
            line,
            "a feed names what happens past its buffer: `, drop oldest`, `, drop newest` or `, refuse`",
            "whenever",
        );
    };
    let (cap, policy) = clause.split_at(at_policy);
    let overflow = match *after(policy, 1) {
        [ref drop, ref which] if is_ident(drop, "drop") && is_ident(which, "oldest") => {
            Overflow::DropOldest
        }
        [ref drop, ref which] if is_ident(drop, "drop") && is_ident(which, "newest") => {
            Overflow::DropNewest
        }
        [ref only] if is_ident(only, "refuse") => Overflow::Refuse,
        _ => {
            return refuse_at(
                line,
                "past its buffer a feed does one of three things: `drop oldest`, `drop newest` or `refuse`",
                "whenever",
            );
        }
    };
    if pattern.is_empty() || batches.is_empty() {
        return refuse_at(line, form, "whenever");
    }
    let cap = bound(cap, line, "buffering", MAX_BUFFER)?;
    let label = labels.next(&format!("whenever:{}", text(pattern)));
    let mut run_shapes = Vec::new();
    let batches = rewrite(batches, labels, &mut run_shapes, line)?;
    let (body, inner) = word_body(children, false)?;
    run_shapes.extend(inner);
    let map = shape(
        Kind::Whenever,
        text(clause),
        text(&line.tokens),
        line,
        run_shapes,
    );
    let feed = Whenever {
        site: Site::of(&map),
        label,
        pattern: pattern.iter().cloned().collect::<TokenStream>(),
        batches,
        cap,
        overflow,
        body,
    };
    Ok((Construct::Whenever(feed), map))
}

/// `justify with (<rules>):`.
pub(super) fn justify(
    line: &Line,
    children: &[LineNode],
    labels: &mut Labels,
) -> Result<(Construct, StepShape)> {
    let rules = match *after(&line.tokens, 1) {
        [ref with, ref rules] if is_ident(with, "with") && is_parens(rules) => {
            group(rules).map(|inner| inner.stream())
        }
        _ => None,
    };
    let Some(rules) = rules.filter(|stream| !stream.is_empty()) else {
        return refuse_at(
            line,
            "`justify` reads `justify with (rules):`, where `rules` is a `Justify`",
            "justify",
        );
    };
    let label = labels.next("justify");
    let (governed, inner) = word_body(children, false)?;
    let subject = text(&rules.clone().into_iter().collect::<Vec<_>>());
    let map = shape(Kind::Justify, subject, text(&line.tokens), line, inner);
    let block = Justify {
        site: Site::of(&map),
        label,
        rules,
        governed,
    };
    Ok((Construct::Justify(block), map))
}

/// The producer `admit` names after `from`.
fn producer_of(token: &TokenTree) -> Option<Producer> {
    let name = ident(token)?.to_string();
    match name.as_str() {
        "model" => Some(Producer::Model),
        "tool" => Some(Producer::Tool),
        "document" => Some(Producer::Document),
        _ => None,
    }
}

/// `admit from <producer> through (<gate>):`, whose block's value is the
/// payload.
pub(super) fn admit(
    line: &Line,
    children: &[LineNode],
    labels: &mut Labels,
) -> Result<(Construct, StepShape)> {
    let form = "`admit` reads `admit from model through (gate):`, or `from tool`, `from document`";
    let (producer, gate) = match *after(&line.tokens, 1) {
        [ref from, ref who, ref through, ref gate]
            if is_ident(from, "from") && is_ident(through, "through") && is_parens(gate) =>
        {
            (producer_of(who), group(gate).map(|inner| inner.stream()))
        }
        _ => (None, None),
    };
    let Some(gate) = gate.filter(|stream| !stream.is_empty()) else {
        return refuse_at(line, form, "admit");
    };
    let Some(producer) = producer else {
        return refuse_at(
            line,
            "`admit` reads from one of three producers: `model`, `tool` or `document`",
            "admit",
        );
    };
    let label = labels.next("admit");
    let (payload, inner) = word_body(children, false)?;
    if payload.tail().is_none() {
        return refuse_at(
            line,
            "the last line under `admit` is the payload's bytes, and this block ends without one",
            "admit",
        );
    }
    let subject = text(&gate.clone().into_iter().collect::<Vec<_>>());
    let map = shape(Kind::Admit, subject, text(&line.tokens), line, inner);
    let admitted = Admit {
        site: Site::of(&map),
        label,
        producer,
        gate,
        payload,
    };
    Ok((Construct::Admit(admitted), map))
}

/// `ready <pattern> from (<readiness>) within <duration>:`.
pub(super) fn ready(
    line: &Line,
    children: &[LineNode],
    labels: &mut Labels,
) -> Result<(Construct, StepShape)> {
    let form = "`ready` reads `ready <name> from (readiness) within 30s:`";
    let tokens = after(&line.tokens, 1);
    let Some(at_from) = tokens.iter().position(|token| is_ident(token, "from")) else {
        return refuse_at(line, form, "ready");
    };
    let (pattern, at_keyword) = tokens.split_at(at_from);
    let (readiness, limit) = match *after(at_keyword, 1) {
        [ref readiness, ref within, ref limit @ ..]
            if is_parens(readiness) && is_ident(within, "within") && !limit.is_empty() =>
        {
            (group(readiness).map(|inner| inner.stream()), limit)
        }
        _ => {
            return refuse_at(
                line,
                "a `ready` waits under a deadline: `ready <name> from (readiness) within 30s:`",
                "ready",
            );
        }
    };
    let Some(readiness) = readiness.filter(|stream| !stream.is_empty()) else {
        return refuse_at(line, form, "ready");
    };
    if pattern.is_empty() {
        return refuse_at(line, form, "ready");
    }
    let deadline = duration(limit, line)?;
    let label = labels.next("ready");
    let (body, inner) = word_body(children, false)?;
    let map = shape(Kind::Ready, text(limit), text(&line.tokens), line, inner);
    let waited = Ready {
        site: Site::of(&map),
        label,
        pattern: pattern.iter().cloned().collect::<TokenStream>(),
        readiness,
        limit: deadline,
        body,
    };
    Ok((Construct::Ready(waited), map))
}

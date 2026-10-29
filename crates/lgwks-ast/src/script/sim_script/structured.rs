//! The grammar generator's structured words: `graph` and its `node`s, `saga`
//! and its `compensate` lines, `whenever`, `justify`, `remember`, `admit` and
//! `ready`, each drawn with the shapes the parser accepts, so the band says
//! every word of the lexicon.

use std::fmt;

use super::super::lexicon::Kind;
use super::Script;

/// Why a structured word could not be drawn.
#[derive(Debug)]
pub(super) enum Drawn {
    /// The seeded stream refused a draw, with its reason.
    Draw(String),
}

impl fmt::Display for Drawn {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Draw(ref reason) => {
                write!(formatter, "a structured word was not drawn: {reason}")
            }
        }
    }
}

impl From<String> for Drawn {
    fn from(reason: String) -> Self {
        Self::Draw(reason)
    }
}

impl From<Drawn> for String {
    fn from(drawn: Drawn) -> Self {
        drawn.to_string()
    }
}

impl Script {
    /// One structured word at `depth`, bound with `let`, with its body.
    pub(super) fn structured(&mut self, depth: usize) -> Result<(), Drawn> {
        self.say(Kind::Let);
        match self.draw.index(7)? {
            0 => self.graph(depth),
            1 => self.saga(depth),
            2 => self.whenever(depth),
            3 => self.justify(depth),
            4 => self.remember(depth),
            5 => self.admit(depth),
            _ => self.ready(depth),
        }
    }

    /// `remember name:` around a block of statements.
    fn remember(&mut self, depth: usize) -> Result<(), Drawn> {
        let name = self.name();
        self.line(
            depth,
            format!("let r{name} = remember answer{name}:"),
            Some(Kind::Remember),
        );
        Ok(self.statements(depth.saturating_add(1))?)
    }

    /// `admit` from each of the three producers, its payload the last line.
    fn admit(&mut self, depth: usize) -> Result<(), Drawn> {
        let name = self.name();
        let producer = *self.draw.pick(&["model", "tool", "document"])?;
        self.line(
            depth,
            format!("let a{name} = admit from {producer} through (site.gate()):"),
            Some(Kind::Admit),
        );
        self.line(depth.saturating_add(1), format!("site.reply({name})"), None);
        Ok(())
    }

    /// `ready` on a readiness under a deadline, its body using the release.
    fn ready(&mut self, depth: usize) -> Result<(), Drawn> {
        let name = self.name();
        let deadline = *self.draw.pick(&["250ms", "2s", "(site.deadline())"])?;
        self.line(
            depth,
            format!("let w{name} = ready db{name} from (site.db()) within {deadline}:"),
            Some(Kind::Ready),
        );
        self.line(
            depth.saturating_add(1),
            format!("site.size(db{name})"),
            None,
        );
        Ok(())
    }

    /// `graph:` with one to three nodes, each needing some of the nodes
    /// declared before it, so the needs never cycle.
    fn graph(&mut self, depth: usize) -> Result<(), Drawn> {
        let name = self.name();
        let header = if self.draw.one_in(2)? {
            "graph"
        } else {
            "graph, at most (site.quota()) at once"
        };
        self.line(depth, format!("let g{name} = {header}:"), Some(Kind::Graph));
        let inner = depth.saturating_add(1);
        let mut declared: Vec<u32> = Vec::new();
        for _ in 0..self.draw.between(1, 3)? {
            let node = self.name();
            let mut needs = Vec::new();
            for earlier in &declared {
                if self.draw.one_in(2)? {
                    needs.push(format!("n{earlier}"));
                }
            }
            let text = if needs.is_empty() {
                format!("node n{node}:")
            } else {
                format!("node n{node} needs {}:", needs.join(", "))
            };
            self.line(inner, text, Some(Kind::Node));
            self.line(inner.saturating_add(1), format!("site.size({node})"), None);
            declared.push(node);
        }
        Ok(())
    }

    /// `saga name:` with a step, its `compensate` line, and sometimes a later
    /// step that has none.
    fn saga(&mut self, depth: usize) -> Result<(), Drawn> {
        let name = self.name();
        self.line(
            depth,
            format!("let s{name} = saga booking{name}:"),
            Some(Kind::Saga),
        );
        let inner = depth.saturating_add(1);
        let body = inner.saturating_add(1);
        let step = self.name();
        self.line(inner, format!("step part{step}:"), Some(Kind::Step));
        self.line(body, format!("site.charge({step}).await.or_retry()?"), None);
        self.line(
            inner,
            format!("compensate part{step}:"),
            Some(Kind::Compensate),
        );
        self.line(body, format!("site.refund({step}).await.or_retry()?"), None);
        if self.draw.one_in(2)? {
            let last = self.name();
            self.line(inner, format!("step part{last}:"), Some(Kind::Step));
            self.line(body, format!("site.size({last})"), None);
        }
        Ok(())
    }

    /// `whenever` over a feed, with each of the three overflow policies.
    fn whenever(&mut self, depth: usize) -> Result<(), Drawn> {
        let name = self.name();
        let policy = *self.draw.pick(&["drop oldest", "drop newest", "refuse"])?;
        self.line(
            depth,
            format!(
                "let w{name} = whenever tick in site.feed({name}).await, buffering 16, {policy}:"
            ),
            Some(Kind::Whenever),
        );
        self.line(depth.saturating_add(1), "site.size(tick)".to_owned(), None);
        Ok(())
    }

    /// `justify with (rules):` around a block of statements.
    fn justify(&mut self, depth: usize) -> Result<(), Drawn> {
        let name = self.name();
        self.line(
            depth,
            format!("let j{name} = justify with (site.rules()):"),
            Some(Kind::Justify),
        );
        Ok(self.statements(depth.saturating_add(1))?)
    }
}

//! Reading the two files the parity check compares: the workflow's jobs and
//! steps, and the lane table's `[toolchain]` and `[[lane]]` entries.
//!
//! Neither is a general parser. Each reads exactly the constructs its file
//! uses, and the parity check's own mutation tests are what show a missed
//! construct: a reader that dropped a step or a lane would fail them.

/// One workflow step that runs a command.
pub(super) struct Step {
    /// The step's `name:`, or `<job>:run[<n>]` for an unnamed one.
    pub(super) name: String,
    /// The command text.
    pub(super) run: String,
    /// The name of the job the step belongs to.
    pub(super) job: String,
}

/// One workflow job.
pub(super) struct Job {
    /// The job's `name:`, or its id.
    pub(super) name: String,
    /// The steps that run commands.
    pub(super) steps: Vec<Step>,
}

/// One `[[lane]]` entry of the lane table.
pub(super) struct Lane {
    /// The stable lane name.
    pub(super) id: String,
    /// The surfaces the lane belongs to: `local`, `ci`, or both.
    pub(super) surfaces: Vec<String>,
    /// The exact shell command, if the lane carries one.
    pub(super) command: Option<String>,
    /// The name of a check implemented inside the coordinator.
    pub(super) builtin: Option<String>,
    /// The workflow step that runs the lane.
    pub(super) ci_step: Option<String>,
    /// The workflow job that runs the lane as unnamed steps.
    pub(super) ci_job: Option<String>,
}

/// The lane table.
pub(super) struct Manifest {
    /// `[toolchain] rust`, or empty when absent.
    pub(super) rust: String,
    /// Every lane, in order.
    pub(super) lanes: Vec<Lane>,
}

/// The state of the step being read.
struct Reading {
    /// The jobs finished so far.
    jobs: Vec<Job>,
    /// The current job's id, once one has started.
    job_id: Option<String>,
    /// The current job's `name:`.
    job_name: String,
    /// The current job's steps so far.
    steps: Vec<Step>,
    /// Whether the `steps:` list has started.
    in_steps: bool,
    /// The name of the step whose `run:` is next.
    pending_name: Option<String>,
    /// The lines of the `run:` body so far.
    run_lines: Vec<String>,
    /// Whether a block `run:` is being collected.
    in_run: bool,
    /// Whether the block folds its lines (`>`) rather than keeping them (`|`).
    folded: bool,
    /// How many run steps the job has had.
    run_index: usize,
}

impl Reading {
    /// An empty reading.
    fn new() -> Self {
        Self {
            jobs: Vec::new(),
            job_id: None,
            job_name: String::new(),
            steps: Vec::new(),
            in_steps: false,
            pending_name: None,
            run_lines: Vec::new(),
            in_run: false,
            folded: false,
            run_index: 0,
        }
    }

    /// The current job's id, or empty before the first job.
    fn id(&self) -> String {
        match self.job_id.as_deref() {
            Some(id) => id.to_owned(),
            None => String::new(),
        }
    }

    /// The current job's display name: its `name:`, else its id.
    fn display_name(&self) -> String {
        if self.job_name.is_empty() {
            self.id()
        } else {
            self.job_name.clone()
        }
    }

    /// Ends the `run:` being collected, if any, as a step.
    fn flush_run(&mut self) {
        if !self.in_run && self.run_lines.is_empty() {
            return;
        }
        let name = match self.pending_name.take() {
            Some(name) => name,
            None => format!("{}:run[{}]", self.id(), self.run_index),
        };
        let run = if self.folded {
            self.run_lines
                .iter()
                .map(|line| line.trim_end())
                .filter(|line| !line.is_empty())
                .collect::<Vec<_>>()
                .join(" ")
        } else {
            self.run_lines.join("\n")
        };
        let job = self.display_name();
        self.steps.push(Step { name, run, job });
        self.run_index = self.run_index.saturating_add(1);
        self.run_lines.clear();
        self.in_run = false;
        self.folded = false;
    }

    /// Ends the current job.
    fn flush_job(&mut self) {
        if self.job_id.is_none() {
            return;
        }
        self.flush_run();
        let name = self.display_name();
        let steps = std::mem::take(&mut self.steps);
        self.jobs.push(Job { name, steps });
        self.job_id = None;
    }
}

/// The id of a job header line, `  <id>:` and nothing else.
fn job_header(line: &str) -> Option<&str> {
    let rest = line.strip_prefix("  ")?;
    let id = rest.trim_end().strip_suffix(':')?;
    let valid = !id.is_empty()
        && id
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_' || ch == '-');
    if valid { Some(id) } else { None }
}

/// The value after `key:` on a line indented exactly `indent` spaces.
fn keyed<'a>(line: &'a str, indent: usize, key: &str) -> Option<&'a str> {
    let rest = line.get(indent..)?;
    if line.get(..indent)?.chars().any(|ch| ch != ' ') {
        return None;
    }
    let rest = rest.strip_prefix(key)?.strip_prefix(':')?;
    Some(rest.trim_start())
}

/// Strips surrounding single or double quotes from a scalar.
fn unquote(value: &str) -> String {
    value
        .trim()
        .trim_matches(|ch| ch == '\'' || ch == '"')
        .to_owned()
}

/// Reads the workflow's jobs and their command steps.
pub(super) fn read_workflow(text: &str) -> Vec<Job> {
    let mut state = Reading::new();
    for raw in text.lines() {
        if let Some(id) = job_header(raw) {
            state.flush_job();
            state.job_id = Some(id.to_owned());
            state.job_name.clear();
            state.steps.clear();
            state.in_steps = false;
            state.run_index = 0;
            continue;
        }
        if state.job_id.is_none() {
            continue;
        }
        if let Some(value) = keyed(raw, 4, "name")
            && !value.is_empty()
        {
            state.job_name = unquote(value);
            continue;
        }
        if keyed(raw, 4, "steps").is_some_and(str::is_empty) {
            state.in_steps = true;
            continue;
        }
        if !state.in_steps {
            continue;
        }
        if let Some(value) = raw.strip_prefix("      - name:")
            && !value.trim().is_empty()
        {
            state.flush_run();
            state.pending_name = Some(unquote(value));
            continue;
        }
        if let Some(value) = keyed(raw, 8, "run") {
            state.flush_run();
            if matches!(value, "" | "|" | "|-" | ">" | ">-" | "|+" | ">+") {
                state.in_run = true;
                state.folded = value.starts_with('>');
                state.run_lines.clear();
            } else {
                state.run_lines = vec![value.to_owned()];
                state.in_run = false;
                state.flush_run();
            }
            continue;
        }
        if state.in_run {
            if let Some(body) = raw.strip_prefix("          ") {
                state.run_lines.push(body.to_owned());
            } else if raw.trim().is_empty() {
                state.run_lines.push(String::new());
            } else {
                state.flush_run();
            }
        }
    }
    state.flush_job();
    state.jobs
}

/// Where the lane reader is.
enum Table {
    /// Before any table, or in one the check does not read.
    Other,
    /// Inside `[toolchain]`.
    Toolchain,
    /// Inside the latest `[[lane]]`.
    Lane,
}

/// A TOML basic string's text from `raw` (escapes undone), with line
/// continuations folded for the multi-line form.
fn unescape(raw: &str, multiline: bool) -> String {
    let mut out = String::new();
    let mut chars = raw.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '\\' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('"') => out.push('"'),
            Some('\\') => out.push('\\'),
            Some('\n') if multiline => {
                while chars.peek().is_some_and(|next| next.is_whitespace()) {
                    chars.next();
                }
            }
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
            None => out.push('\\'),
        }
    }
    out
}

/// The text of a one-line basic string at the front of `rest` (after its opening
/// quote), and what follows it.
fn basic_string(rest: &str) -> Option<(String, &str)> {
    let mut escaped = false;
    for (index, ch) in rest.char_indices() {
        if escaped {
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else if ch == '"' {
            let body = rest.get(..index)?;
            let after = rest.get(index.saturating_add(1)..)?;
            return Some((unescape(body, false), after));
        }
    }
    None
}

/// The byte index of an unescaped closing `"""` in `text`.
fn closing_triple(text: &str) -> Option<usize> {
    let mut escaped = false;
    for (index, ch) in text.char_indices() {
        if escaped {
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else if ch == '"' && text.get(index..)?.starts_with("\"\"\"") {
            return Some(index);
        }
    }
    None
}

/// The strings a multi-line basic string holds, starting after its opening `"""`.
fn read_multiline(lines: &[&str], cursor: &mut usize, after: &str) -> Vec<String> {
    let mut body = String::new();
    let mut current = after;
    loop {
        if let Some(end) = closing_triple(current) {
            if let Some(piece) = current.get(..end) {
                body.push_str(piece);
            }
            break;
        }
        body.push_str(current);
        body.push('\n');
        *cursor = cursor.saturating_add(1);
        match lines.get(*cursor) {
            Some(next) => current = next,
            None => break,
        }
    }
    let trimmed = match body.strip_prefix('\n') {
        Some(rest) => rest,
        None => &body,
    };
    vec![unescape(trimmed, true)]
}

/// The strings of an array that starts at `first`, which may continue over lines.
fn read_array(lines: &[&str], cursor: &mut usize, first: &str) -> Vec<String> {
    let mut strings = Vec::new();
    let mut current = first;
    loop {
        let mut rest = current;
        while let Some(open) = rest.find('"') {
            let Some(tail) = rest.get(open.saturating_add(1)..) else {
                break;
            };
            match basic_string(tail) {
                Some((text, after)) => {
                    strings.push(text);
                    rest = after;
                }
                None => break,
            }
        }
        if current.contains(']') {
            break;
        }
        *cursor = cursor.saturating_add(1);
        match lines.get(*cursor) {
            Some(next) => current = next,
            None => break,
        }
    }
    strings
}

/// Reads the value after `=` on `lines[*cursor]`, advancing `cursor` past every
/// line a multi-line string or array consumes. Non-string values read as none.
fn read_value(lines: &[&str], cursor: &mut usize, first: &str) -> Vec<String> {
    let first = first.trim_start();
    if let Some(after) = first.strip_prefix("\"\"\"") {
        return read_multiline(lines, cursor, after);
    }
    if let Some(after) = first.strip_prefix('"') {
        return match basic_string(after) {
            Some((text, _)) => vec![text],
            None => Vec::new(),
        };
    }
    if first.starts_with('[') {
        return read_array(lines, cursor, first);
    }
    Vec::new()
}

/// Reads `[toolchain] rust` and every `[[lane]]` of the lane table.
pub(super) fn read_manifest(text: &str) -> Manifest {
    let lines: Vec<&str> = text.lines().collect();
    let mut manifest = Manifest {
        rust: String::new(),
        lanes: Vec::new(),
    };
    let mut table = Table::Other;
    let mut cursor = 0_usize;
    while let Some(line) = lines.get(cursor) {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            cursor = cursor.saturating_add(1);
            continue;
        }
        if trimmed == "[[lane]]" {
            manifest.lanes.push(Lane {
                id: String::new(),
                surfaces: vec!["local".to_owned()],
                command: None,
                builtin: None,
                ci_step: None,
                ci_job: None,
            });
            table = Table::Lane;
            cursor = cursor.saturating_add(1);
            continue;
        }
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            table = if trimmed == "[toolchain]" {
                Table::Toolchain
            } else {
                Table::Other
            };
            cursor = cursor.saturating_add(1);
            continue;
        }
        let Some((key, value_text)) = trimmed.split_once('=') else {
            cursor = cursor.saturating_add(1);
            continue;
        };
        let key = key.trim();
        let strings = read_value(&lines, &mut cursor, value_text);
        let first = strings.first().cloned();
        match table {
            Table::Toolchain if key == "rust" => {
                if let Some(rust) = first {
                    manifest.rust = rust;
                }
            }
            Table::Lane => {
                if let Some(lane) = manifest.lanes.last_mut() {
                    match key {
                        "id" => {
                            if let Some(id) = first {
                                lane.id = id;
                            }
                        }
                        "surfaces" => {
                            lane.surfaces = strings
                                .iter()
                                .flat_map(|text| text.split_whitespace())
                                .map(str::to_owned)
                                .collect();
                        }
                        "command" => lane.command = first,
                        "builtin" => lane.builtin = first,
                        "ci_step" => lane.ci_step = first,
                        "ci_job" => lane.ci_job = first,
                        _ => {}
                    }
                }
            }
            Table::Toolchain | Table::Other => {}
        }
        cursor = cursor.saturating_add(1);
    }
    manifest
}

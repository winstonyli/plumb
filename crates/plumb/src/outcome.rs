//! What an action reports when it is done or refused. A `Failure` is the content every "could not be done" report shares;
//! the modal and the status line (an `Outcome`) are both drawn from it, so a delete, an end and a suspend read alike.
use crate::fmt;
use plumb_scan::reason::Reason;

/// The detail lines shown before "and N more".
pub const SHOWN: usize = 8;

/// The lines a list of items is shown as, with "and N more" past `SHOWN`.
pub fn lines(items: &[String]) -> Vec<String> {
    fmt::path_list(items, SHOWN).lines().map(str::to_string).collect()
}

/// The status line's leading message: what happened, whether it is good news (a problem is drawn in the danger colour),
/// and a line per item for the tooltip.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Outcome {
    pub line: String,
    pub ok: bool,
    pub detail: Vec<String>,
}

impl Outcome {
    pub fn ok(line: impl Into<String>) -> Self {
        Self { line: line.into(), ok: true, detail: Vec::new() }
    }

    /// A refusal or a failure.
    pub fn problem(line: impl Into<String>) -> Self {
        Self { line: line.into(), ok: false, detail: Vec::new() }
    }

    pub fn is_empty(&self) -> bool {
        self.line.is_empty()
    }

    /// Adds a further problem to the line ("...; 2 skipped: why") and its detail lines.
    pub fn and(mut self, line: &str, detail: Vec<String>) -> Self {
        self.ok = false;
        if self.line.is_empty() {
            self.line = line.to_string();
        } else {
            self.line += &format!("; {line}");
        }
        self.detail.extend(detail);
        self
    }
}

/// The parts every "could not be done" report shares: one item gets "Could not <verb> <it>" and its reason as a sentence;
/// several get "N of M not <past>" and a line per item. `hint` follows the reason(s).
#[derive(Clone, Debug, PartialEq)]
pub struct Failure {
    pub heading: String,
    pub body: Vec<String>,
    pub detail: Vec<String>,
    /// "<subject>: <reason>" per item, for Copy.
    pub copy: String,
    /// The first item's reason, which the status line gives.
    why: String,
}

impl Failure {
    /// `failed` is (subject, reason) out of `total` items tried.
    pub fn new(verb: &str, past: &str, failed: &[(String, String)], total: usize, hint: &str) -> Self {
        let each: Vec<String> = failed.iter().map(|(subject, why)| format!("{subject}: {why}")).collect();
        let copy = each.join("\n");
        let why = failed.first().map(|f| f.1.clone()).unwrap_or_default();
        match failed {
            [(subject, why)] if total == 1 => Self { heading: format!("Could not {verb} {subject}"), body: vec![sentence(why, hint)], detail: Vec::new(), copy, why: why.clone() },
            _ => Self { heading: format!("{} of {total} not {past}", failed.len()), body: vec![hint.trim().to_string()], detail: lines(&each), copy, why },
        }
    }

    /// The same report for the status line: the heading in lower case, the reason, then `tail` ("; still marked").
    pub fn outcome(&self, tail: &str) -> Outcome {
        let mut c = self.heading.chars();
        let head: String = c.next().map(|f| f.to_lowercase().collect()).unwrap_or_default();
        Outcome { line: format!("{head}{}: {}{tail}", c.as_str(), self.why), ok: false, detail: self.detail.clone() }
    }
}

fn sentence(reason: &str, hint: &str) -> String {
    let mut c = reason.chars();
    let first: String = c.next().map(|f| f.to_uppercase().collect()).unwrap_or_default();
    format!("{first}{}.{hint}", c.as_str())
}

/// What to do about a refusal, for the process reports: only "access denied" has a remedy.
pub fn admin_hint<'a>(reasons: impl IntoIterator<Item = &'a Reason>) -> &'static str {
    if reasons.into_iter().any(Reason::is_access_denied) { " It may be protected or elevated; run plumb as administrator to reach more processes." } else { "" }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair(a: &str, b: &str) -> (String, String) {
        (a.into(), b.into())
    }

    #[test]
    fn one_item_and_several_read_the_same_on_the_status_line_as_in_the_dialog() {
        let one = Failure::new("delete", "deleted", &[pair("C:\\f", "in use")], 1, "");
        assert_eq!(one.heading, "Could not delete C:\\f");
        assert_eq!(one.outcome("; still marked").line, "could not delete C:\\f: in use; still marked");
        let some = Failure::new("end", "ended", &[pair("a", "x"), pair("b", "y")], 3, "");
        assert_eq!(some.heading, "2 of 3 not ended");
        let o = some.outcome("");
        assert_eq!((o.line.as_str(), o.ok, o.detail.len()), ("2 of 3 not ended: x", false, 2));
    }

    #[test]
    fn a_further_problem_extends_the_line_and_the_detail() {
        let o = Outcome::ok("deleted 3 items").and("1 skipped: protected", vec!["C:\\w: protected".into()]);
        assert_eq!((o.line.as_str(), o.ok, o.detail.len()), ("deleted 3 items; 1 skipped: protected", false, 1));
        assert_eq!(Outcome::default().and("x", Vec::new()).line, "x");
    }
}

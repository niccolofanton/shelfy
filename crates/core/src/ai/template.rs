//! The template language of the prompt files in `shared/ai/` (`*.md`), the port of
//! `shared/ai/template.ts`. The golden set `ai/catalog/templates` keeps the two
//! renderers identical.
//!
//! A template is the file's text without its final line break, read line by line:
//!
//! - `{{#if name}}` … `{{else}}` … `{{/if}}` (or `#unless`), each on a line of its
//!   own: the lines between are kept when `name` is true or a non-empty text
//!   (`#unless`: when it is not). Sections nest; `{{else}}` is optional.
//! - `{{! a comment }}`: a line of its own, never output.
//! - `{{name}}` anywhere in any other line: the variable's text, inserted as it
//!   is. A value is never read again as template syntax, so untrusted text
//!   cannot inject any.
//!
//! The kept lines are joined with `\n`. It is an error when the template uses a
//! variable the caller did not give (in any branch), when a section is
//! unbalanced, or when a directive is not alone on its line.

use std::sync::LazyLock;

use regex::Regex;

/// A template variable.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Var<'a> {
    /// A text, for `{{name}}` and for sections (true when not empty).
    Text(&'a str),
    /// A flag, for sections only.
    Flag(bool),
}

/// A template that cannot be rendered with the given variables.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("line {line}: {message}")]
pub struct TemplateError {
    /// The 1-based line of the template.
    pub line: usize,
    /// What is wrong.
    pub message: String,
}

// Blanks are spaces and tabs only, as in template.ts (no Unicode `\s`, whose
// sets differ between JavaScript and Rust).
static OPEN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\{\{[ \t]*#(if|unless)[ \t]+([A-Za-z][A-Za-z0-9_]*)[ \t]*\}\}$")
        .expect("valid pattern")
});
static ELSE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\{\{[ \t]*else[ \t]*\}\}$").expect("valid pattern"));
static CLOSE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\{\{[ \t]*/(if|unless)[ \t]*\}\}$").expect("valid pattern"));
static COMMENT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?s)^\{\{!.*\}\}$").expect("valid pattern"));
static VARIABLE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\{\{[ \t]*([A-Za-z][A-Za-z0-9_]*)[ \t]*\}\}").expect("valid pattern")
});
static STRAY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\{\{[ \t]*(?:[#/!]|else[ \t]*\}\})").expect("valid pattern"));

struct Section {
    unless: bool,
    /// Whether the enclosing lines are output.
    outer: bool,
    /// Whether the condition holds (for `unless`: does not hold).
    holds: bool,
    in_else: bool,
    line: usize,
}

fn error(line: usize, message: impl Into<String>) -> TemplateError {
    TemplateError {
        line,
        message: message.into(),
    }
}

fn lookup<'v>(vars: &[(&str, Var<'v>)], name: &str, line: usize) -> Result<Var<'v>, TemplateError> {
    vars.iter()
        .find(|(n, _)| *n == name)
        .map(|(_, v)| *v)
        .ok_or_else(|| error(line, format!("unknown variable \"{name}\"")))
}

/// Renders `template` (a prompt file's text) with `vars`.
///
/// # Errors
///
/// [`TemplateError`] for an unknown variable, a flag used as text, an
/// unbalanced section or a directive that is not alone on its line.
pub fn render(template: &str, vars: &[(&str, Var<'_>)]) -> Result<String, TemplateError> {
    let text = template.strip_suffix('\n').unwrap_or(template);
    let mut out: Vec<String> = Vec::new();
    let mut stack: Vec<Section> = Vec::new();
    let mut on = true;
    for (index, line) in text.split('\n').enumerate() {
        let n = index + 1;
        let bare = line.trim_matches(|c| c == ' ' || c == '\t');
        if let Some(open) = OPEN.captures(bare) {
            let truthy = match lookup(vars, &open[2], n)? {
                Var::Flag(flag) => flag,
                Var::Text(text) => !text.is_empty(),
            };
            let unless = &open[1] == "unless";
            let holds = truthy != unless;
            stack.push(Section {
                unless,
                outer: on,
                holds,
                in_else: false,
                line: n,
            });
            on = on && holds;
            continue;
        }
        if ELSE.is_match(bare) {
            match stack.last_mut() {
                Some(top) if !top.in_else => {
                    top.in_else = true;
                    on = top.outer && !top.holds;
                }
                _ => return Err(error(n, "{{else}} outside a section, or a second one")),
            }
            continue;
        }
        if let Some(close) = CLOSE.captures(bare) {
            let kind = &close[1];
            let top = stack
                .pop()
                .ok_or_else(|| error(n, format!("{{{{/{kind}}}}} closes no section")))?;
            let open_kind = if top.unless { "unless" } else { "if" };
            if open_kind != kind {
                return Err(error(
                    n,
                    format!(
                        "{{{{/{kind}}}}} closes {{{{#{open_kind}}}}} of line {}",
                        top.line
                    ),
                ));
            }
            on = top.outer;
            continue;
        }
        if COMMENT.is_match(bare) {
            continue;
        }
        if STRAY.is_match(line) {
            return Err(error(n, "a directive must be alone on its line"));
        }
        // Every variable must exist, in every branch; only kept lines are filled in.
        let mut rendered = String::with_capacity(line.len());
        let mut last = 0;
        for found in VARIABLE.captures_iter(line) {
            let whole = found.get(0).expect("group 0 always matches");
            let name = &found[1];
            match lookup(vars, name, n)? {
                Var::Text(text) => {
                    rendered.push_str(&line[last..whole.start()]);
                    rendered.push_str(text);
                    last = whole.end();
                }
                Var::Flag(_) => return Err(error(n, format!("\"{name}\" is a flag, not a text"))),
            }
        }
        rendered.push_str(&line[last..]);
        if on {
            out.push(rendered);
        }
    }
    if let Some(open) = stack.pop() {
        let kind = if open.unless { "unless" } else { "if" };
        return Err(error(open.line, format!("{{{{#{kind}}}}} is not closed")));
    }
    Ok(out.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::Var::{Flag, Text};
    use super::*;

    fn ok(template: &str, vars: &[(&str, Var<'_>)]) -> String {
        render(template, vars).unwrap()
    }

    fn err(template: &str, vars: &[(&str, Var<'_>)]) -> String {
        render(template, vars).unwrap_err().to_string()
    }

    #[test]
    fn plain_text_loses_its_final_line_break() {
        assert_eq!(ok("one\ntwo\n", &[]), "one\ntwo");
        assert_eq!(ok("one\ntwo", &[]), "one\ntwo");
        assert_eq!(ok("one\n\n", &[]), "one\n");
        assert_eq!(ok("", &[]), "");
        assert_eq!(ok("a\n\n  b  \n\tc\n", &[]), "a\n\n  b  \n\tc");
    }

    #[test]
    fn sections_follow_flags_and_texts() {
        let t = "start\n{{#if x}}\nyes\n{{else}}\nno\n{{/if}}\nend\n";
        assert_eq!(ok(t, &[("x", Flag(true))]), "start\nyes\nend");
        assert_eq!(ok(t, &[("x", Text("v"))]), "start\nyes\nend");
        assert_eq!(ok(t, &[("x", Flag(false))]), "start\nno\nend");
        assert_eq!(ok(t, &[("x", Text(""))]), "start\nno\nend");
        let unless = "{{#unless x}}\nshown\n{{/unless}}\n";
        assert_eq!(ok(unless, &[("x", Flag(false))]), "shown");
        assert_eq!(ok(unless, &[("x", Flag(true))]), "");
    }

    #[test]
    fn sections_nest() {
        let t =
            "{{#if a}}\nA\n{{#if b}}\nAB\n{{else}}\nA-not-B\n{{/if}}\n{{else}}\nnot-A\n{{/if}}\n";
        let vars = |a, b| [("a", Flag(a)), ("b", Flag(b))];
        assert_eq!(ok(t, &vars(true, true)), "A\nAB");
        assert_eq!(ok(t, &vars(true, false)), "A\nA-not-B");
        assert_eq!(ok(t, &vars(false, true)), "not-A");
    }

    #[test]
    fn blanks_comments_and_literal_braces() {
        let t = "  {{ #if x }}\t\n{{ x }}\n {{ else }}\n-\n{{ /if }}\n";
        assert_eq!(ok(t, &[("x", Text("v"))]), "v");
        assert_eq!(ok("{{! a note }}\ntext\n{{!another}}\n", &[]), "text");
        assert_eq!(
            ok("{alias, canonical} {{ 2 }} {x}\n", &[]),
            "{alias, canonical} {{ 2 }} {x}"
        );
    }

    #[test]
    fn values_are_never_read_as_syntax() {
        let value = "{{w}} {{#if w}}\n{{/if}} $& $1";
        assert_eq!(
            ok("before {{v}} after\n", &[("v", Text(value))]),
            format!("before {value} after")
        );
        assert_eq!(
            ok("{{a}}{{b}}\n", &[("a", Text("1")), ("b", Text("2"))]),
            "12"
        );
    }

    #[test]
    fn errors() {
        assert_eq!(
            err("{{missing}}\n", &[]),
            "line 1: unknown variable \"missing\""
        );
        assert!(err("{{#if x}}\n{{typo}}\n{{/if}}\n", &[("x", Flag(false))]).starts_with("line 2"));
        assert!(err("{{x}}\n", &[("x", Flag(true))]).contains("is a flag"));
        assert!(err("{{#if x}}\na\n", &[("x", Flag(true))]).contains("not closed"));
        assert!(err("a\n{{/if}}\n", &[]).contains("closes no section"));
        assert!(err("{{else}}\n", &[]).contains("outside a section"));
        assert!(
            err(
                "{{#if x}}\n{{else}}\n{{else}}\n{{/if}}\n",
                &[("x", Flag(true))]
            )
            .contains("outside a section")
        );
        assert!(err("{{#if x}}\n{{/unless}}\n", &[("x", Flag(true))]).contains("closes"));
        for inline in [
            "a {{#if x}}\n{{/if}}\n",
            "{{#if x}} b\n{{/if}}\n",
            "text {{! note }}\n",
            "x {{else}}\n",
        ] {
            assert!(
                err(inline, &[("x", Flag(true))]).contains("alone"),
                "{inline}"
            );
        }
    }
}

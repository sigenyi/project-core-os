//! GBNF grammar generation for llama.cpp.
//!
//! The grammar is derived from the action catalog, so the model is physically unable
//! to sample anything except one well-formed intent naming an *enabled* action with
//! correctly typed arguments. It mirrors the validators in [`crate::validate`] as
//! closely as GBNF allows; the validators remain authoritative.

use std::collections::BTreeMap;
use std::fmt::Write;

use crate::catalog::{ActionSpec, CATALOG, ParamKind, ParamSpec};

#[derive(Debug, Clone)]
pub struct GrammarOptions<'a> {
    /// Restrict the grammar to these action names (`None` = the whole catalog).
    pub actions: Option<&'a [&'a str]>,
    /// Maximum length of the `thought` field; 0 omits the field entirely.
    pub thought_max: usize,
}

impl Default for GrammarOptions<'_> {
    fn default() -> Self {
        GrammarOptions { actions: None, thought_max: 200 }
    }
}

/// Shared rules, emitted only when referenced.
const SHARED: &[(&str, &str)] = &[
    ("ws", r#"" "?"#),
    ("bool", r#""true" | "false""#),
    ("jchar", r#"[^"\\\x00-\x1F] | "\\" (["\\/bfnrt] | "u" [0-9a-fA-F]{4})"#),
    ("text", r#""\"" jchar+ "\"""#),
    ("pchar", r#"[^"\\\x00-\x1F]"#),
    ("secret", r#""\"" [\x20\x21\x23-\x5B\x5D-\x7E]{8,63} "\"""#),
    ("v-service", r#""\"" [A-Za-z0-9] [A-Za-z0-9_.:@-]* "\"""#),
    ("v-package", r#""\"" [A-Za-z0-9] [A-Za-z0-9@._+-]* "\"""#),
    ("v-query", r#""\"" [A-Za-z0-9] [A-Za-z0-9 ._+-]{0,63} "\"""#),
    ("v-module", r#""\"" [A-Za-z0-9] [A-Za-z0-9_-]{0,63} "\"""#),
    ("v-path", r#""\"/" pchar* "\"""#),
    ("v-hostname", r#""\"" [A-Za-z0-9] [A-Za-z0-9.-]* "\"""#),
    ("v-host", r#""\"" [A-Za-z0-9:] [A-Za-z0-9.:-]* "\"""#),
    ("v-timezone", r#""\"" [A-Za-z0-9] [A-Za-z0-9_+/-]{0,63} "\"""#),
    ("v-ssid", r#""\"" [^"\\\x00-\x1F\x2D] pchar{0,31} "\"""#),
    ("v-iface", r#""\"" [A-Za-z0-9] [A-Za-z0-9_.:-]{0,14} "\"""#),
    ("v-program", r#""\"" [A-Za-z0-9] [A-Za-z0-9_.+-]{0,63} "\"""#),
    ("v-arg", r#""\"" pchar+ "\"""#),
];

/// Rules each shared rule depends on.
fn dependencies(rule: &str) -> &'static [&'static str] {
    match rule {
        "text" => &["jchar"],
        "v-path" | "v-ssid" | "v-arg" => &["pchar"],
        _ => &[],
    }
}

struct Builder {
    used: BTreeMap<&'static str, ()>,
}

impl Builder {
    fn use_rule(&mut self, name: &'static str) -> &'static str {
        if self.used.insert(name, ()).is_none() {
            for dep in dependencies(name) {
                self.use_rule(dep);
            }
        }
        name
    }

    fn value(&mut self, kind: &ParamKind) -> String {
        match kind {
            ParamKind::Text { .. } => self.use_rule("text").into(),
            ParamKind::Secret { .. } => self.use_rule("secret").into(),
            ParamKind::Integer { min, max } => integer_rule(*min, *max),
            ParamKind::Boolean => self.use_rule("bool").into(),
            ParamKind::Choice(values) => {
                let alts: Vec<String> = values.iter().map(|v| format!(r#""\"{v}\"""#)).collect();
                format!("({})", alts.join(" | "))
            }
            ParamKind::ServiceName => self.use_rule("v-service").into(),
            ParamKind::PackageName => self.use_rule("v-package").into(),
            ParamKind::PackageQuery => self.use_rule("v-query").into(),
            ParamKind::KernelModule => self.use_rule("v-module").into(),
            ParamKind::Path => self.use_rule("v-path").into(),
            ParamKind::Hostname => self.use_rule("v-hostname").into(),
            ParamKind::HostTarget => self.use_rule("v-host").into(),
            ParamKind::Timezone => self.use_rule("v-timezone").into(),
            ParamKind::Ssid => self.use_rule("v-ssid").into(),
            ParamKind::Interface => self.use_rule("v-iface").into(),
            ParamKind::Program => self.use_rule("v-program").into(),
            ParamKind::ArgList { max_items } => {
                let arg = self.use_rule("v-arg");
                self.use_rule("ws");
                let more = max_items.saturating_sub(1);
                format!(r#""[" ({arg} ("," ws {arg}){{0,{more}}})? "]""#)
            }
        }
    }

    /// Grammar for an ordered parameter list in which optional parameters may be
    /// omitted. Commas are placed correctly whichever subset is present.
    fn sequence(&mut self, params: &[ParamSpec], need_comma: bool) -> String {
        let Some((p, rest)) = params.split_first() else { return String::new() };
        let comma = if need_comma { r#""," ws "# } else { "" };
        let item = format!(r#"{comma}"\"{}\":" ws {}"#, p.name, self.value(&p.kind));
        let tail = self.sequence(rest, true);
        let with = join(&item, &tail);
        if p.required {
            return with;
        }
        let without = self.sequence(rest, need_comma);
        if without.is_empty() { format!("({with})?") } else { format!("({with} | {without})") }
    }

    fn action_rule(&mut self, spec: &ActionSpec) -> String {
        self.use_rule("ws");
        let body = self.sequence(spec.params, false);
        let args = if body.is_empty() { r#""{}""#.to_string() } else { format!(r#""{{" ws {body} ws "}}""#) };
        format!(r#""\"action\":" ws "\"{}\"," ws "\"args\":" ws {args}"#, spec.name)
    }
}

fn join(a: &str, b: &str) -> String {
    match (a.is_empty(), b.is_empty()) {
        (true, _) => b.to_string(),
        (_, true) => a.to_string(),
        _ => format!("{a} {b}"),
    }
}

/// JSON integers without leading zeros, bounded in digit count by the range.
fn integer_rule(min: i64, max: i64) -> String {
    let digits = min.unsigned_abs().max(max.unsigned_abs()).to_string().len();
    let body = if digits <= 1 { "[0-9]".to_string() } else { format!(r#"("0" | [1-9] [0-9]{{0,{}}})"#, digits - 1) };
    if min < 0 { format!(r#""-"? {body}"#) } else { body }
}

fn rule_name(action: &str) -> String {
    format!("a-{}", action.replace('_', "-"))
}

/// Generate a GBNF grammar accepting exactly one intent object.
pub fn gbnf(options: &GrammarOptions) -> String {
    let specs: Vec<&ActionSpec> =
        CATALOG.iter().filter(|s| options.actions.is_none_or(|allowed| allowed.contains(&s.name))).collect();
    assert!(!specs.is_empty(), "grammar needs at least one action");

    let mut b = Builder { used: BTreeMap::new() };
    b.use_rule("ws");
    let mut out = String::new();
    writeln!(out, "# C.O.R.E. OS intent grammar (generated from the action catalog; do not edit)").unwrap();
    if options.thought_max > 0 {
        b.use_rule("pchar");
        writeln!(out, r#"root ::= "{{" ws "\"thought\":" ws thought "," ws action ws "}}""#).unwrap();
        writeln!(out, r#"thought ::= "\"" pchar{{0,{}}} "\"""#, options.thought_max).unwrap();
    } else {
        writeln!(out, r#"root ::= "{{" ws action ws "}}""#).unwrap();
    }
    let names: Vec<String> = specs.iter().map(|s| rule_name(s.name)).collect();
    writeln!(out, "action ::= {}", names.join(" | ")).unwrap();
    for spec in &specs {
        let rule = b.action_rule(spec);
        writeln!(out, "{} ::= {rule}", rule_name(spec.name)).unwrap();
    }
    for (name, body) in SHARED {
        if b.used.contains_key(name) {
            writeln!(out, "{name} ::= {body}").unwrap();
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule<'a>(grammar: &'a str, name: &str) -> &'a str {
        grammar
            .lines()
            .find_map(|l| l.strip_prefix(&format!("{name} ::= ")))
            .unwrap_or_else(|| panic!("rule {name} missing"))
    }

    #[test]
    fn full_grammar_has_every_action() {
        let g = gbnf(&GrammarOptions::default());
        for spec in CATALOG {
            assert!(g.contains(&format!("{} ::=", rule_name(spec.name))), "{}", spec.name);
        }
        assert!(g.starts_with("# C.O.R.E."));
    }

    #[test]
    fn every_referenced_rule_is_defined() {
        let g = gbnf(&GrammarOptions::default());
        let defined: Vec<&str> = g.lines().filter_map(|l| l.split_once(" ::= ").map(|(n, _)| n)).collect();
        for line in g.lines().filter(|l| !l.starts_with('#')) {
            let (_, body) = line.split_once(" ::= ").unwrap();
            // Strip literals and char classes, then every remaining identifier is a rule.
            let mut cleaned = String::new();
            let mut chars = body.chars().peekable();
            while let Some(c) = chars.next() {
                match c {
                    '"' => {
                        while let Some(d) = chars.next() {
                            if d == '\\' {
                                chars.next();
                            } else if d == '"' {
                                break;
                            }
                        }
                        cleaned.push(' ');
                    }
                    '[' => {
                        while let Some(d) = chars.next() {
                            if d == '\\' {
                                chars.next();
                            } else if d == ']' {
                                break;
                            }
                        }
                        cleaned.push(' ');
                    }
                    '{' => {
                        for d in chars.by_ref() {
                            if d == '}' {
                                break;
                            }
                        }
                        cleaned.push(' ');
                    }
                    _ => cleaned.push(c),
                }
            }
            for ident in cleaned.split(|c: char| !(c.is_ascii_alphanumeric() || c == '-')).filter(|s| !s.is_empty()) {
                assert!(defined.contains(&ident), "undefined rule {ident:?} in: {line}");
            }
        }
    }

    #[test]
    fn restricting_actions_shrinks_the_grammar() {
        let g = gbnf(&GrammarOptions { actions: Some(&["respond", "disk_usage"]), thought_max: 0 });
        assert_eq!(rule(&g, "action"), "a-respond | a-disk-usage");
        assert!(!g.contains("install"));
        assert!(!g.contains("thought"));
        assert!(!g.contains("v-service"), "unused shared rules are omitted");
        assert_eq!(rule(&g, "a-disk-usage"), r#""\"action\":" ws "\"disk_usage\"," ws "\"args\":" ws "{}""#);
    }

    #[test]
    fn optional_parameters_get_correct_commas() {
        let g = gbnf(&GrammarOptions::default());
        let read_logs = rule(&g, "a-read-logs");
        // unit?, priority?, lines? -> any subset, comma only between present items.
        assert!(read_logs.contains(r#"("\"unit\":" ws v-service"#), "{read_logs}");
        assert!(read_logs.contains(r#""," ws "\"priority\":""#), "{read_logs}");
        let enable = rule(&g, "a-enable-service");
        assert_eq!(
            enable,
            r#""\"action\":" ws "\"enable_service\"," ws "\"args\":" ws "{" ws "\"service\":" ws v-service ("," ws "\"now\":" ws bool)? ws "}""#
        );
    }

    #[test]
    fn integers_have_no_leading_zeros() {
        assert_eq!(integer_rule(0, 150), r#"("0" | [1-9] [0-9]{0,2})"#);
        assert_eq!(integer_rule(1, 9), "[0-9]");
        assert_eq!(integer_rule(-5, 5), r#""-"? [0-9]"#);
    }
}

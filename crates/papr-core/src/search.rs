//! Search query language → safe FTS5 MATCH (see `docs/search.md`).
//!
//! Never pass raw user strings to `MATCH`. Parse into an AST, then compile
//! quoted terms with optional column filters and prefix markers.
//!
//! Bare terms may expand via wordcloud entity aliases — see
//! `docs/search-synonyms.md`.

use crate::wordcloud_dict::{SynonymGroup, WordCloudDict};

/// How adjacent bare terms combine when no explicit `OR` / `AND` is present.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchMode {
    /// UI / list search: space = AND (narrowing).
    Strict,
    /// RAG / CLI default: space = OR (recall).
    Recall,
}

/// Result of compiling a user query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompiledSearch {
    /// FTS5 MATCH expression, or `None` when the query should match nothing
    /// (empty, punctuation-only, or unrecoverable).
    pub match_expr: Option<String>,
    /// `feed:name` filters — case-insensitive prefix on `feeds.title`.
    pub feed_prefixes: Vec<String>,
}

impl CompiledSearch {
    /// True when neither FTS nor feed filters can select rows.
    pub fn is_empty(&self) -> bool {
        self.match_expr.is_none() && self.feed_prefixes.is_empty()
    }
}

/// Compile `input` per `docs/search.md` for the given mode (no synonym expansion).
pub fn compile_search(input: &str, mode: SearchMode) -> CompiledSearch {
    compile_search_with_dict(input, mode, None)
}

/// Like [`compile_search`], optionally expanding bare terms via `dict` aliases.
pub fn compile_search_with_dict(
    input: &str,
    mode: SearchMode,
    dict: Option<&WordCloudDict>,
) -> CompiledSearch {
    let tokens = tokenize(input);
    if tokens.is_empty() {
        return CompiledSearch {
            match_expr: None,
            feed_prefixes: Vec::new(),
        };
    }
    let mut parser = Parser {
        tokens: &tokens,
        pos: 0,
        mode,
        ok: true,
    };
    let Some(ast) = parser.parse_expr() else {
        return CompiledSearch {
            match_expr: None,
            feed_prefixes: Vec::new(),
        };
    };
    if !parser.ok || parser.pos < tokens.len() {
        return CompiledSearch {
            match_expr: None,
            feed_prefixes: Vec::new(),
        };
    }
    let mut feeds = Vec::new();
    collect_feeds(&ast, &mut feeds);
    let match_expr = compile_node(&ast, mode, dict);
    CompiledSearch {
        match_expr,
        feed_prefixes: feeds,
    }
}

/// Convenience for call sites that only need the MATCH string (no `feed:`).
/// Returns a match-nothing expression when empty, matching historical `fts_query`.
pub fn fts_match_expr(input: &str, mode: SearchMode) -> String {
    fts_match_expr_with_dict(input, mode, None)
}

/// Like [`fts_match_expr`] with optional synonym expansion.
pub fn fts_match_expr_with_dict(
    input: &str,
    mode: SearchMode,
    dict: Option<&WordCloudDict>,
) -> String {
    compile_search_with_dict(input, mode, dict)
        .match_expr
        .unwrap_or_else(|| "\"\"".into())
}

// ── tokens ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
enum Tok {
    Term {
        field: Option<Field>,
        text: String,
        phrase: bool,
        /// User wrote an explicit trailing `*`.
        explicit_prefix: bool,
    },
    Or,
    And,
    Not,
    LParen,
    RParen,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Field {
    Title,
    Body,
    Feed,
}

fn tokenize(input: &str) -> Vec<Tok> {
    let mut out = Vec::new();
    let chars: Vec<char> = input.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        if c == '(' {
            out.push(Tok::LParen);
            i += 1;
            continue;
        }
        if c == ')' {
            out.push(Tok::RParen);
            i += 1;
            continue;
        }
        if c == '"' {
            i += 1;
            let mut s = String::new();
            while i < chars.len() && chars[i] != '"' {
                s.push(chars[i]);
                i += 1;
            }
            if i < chars.len() {
                i += 1; // closing quote
            }
            // Optional field prefix was already consumed; phrases have no field
            // unless we saw field: before the quote — handled below via peek.
            out.push(Tok::Term {
                field: None,
                text: s,
                phrase: true,
                explicit_prefix: false,
            });
            continue;
        }
        if c == '-' {
            // Unary minus only at start of a term (not inside words).
            let next_ws_or_end = i + 1 >= chars.len() || chars[i + 1].is_whitespace();
            if !next_ws_or_end {
                out.push(Tok::Not);
                i += 1;
                continue;
            }
            i += 1;
            continue;
        }

        // Read a bare word (possibly field:value or trailing *).
        let start = i;
        while i < chars.len()
            && !chars[i].is_whitespace()
            && chars[i] != '('
            && chars[i] != ')'
            && chars[i] != '"'
        {
            // Stop before a unary-minus that starts a new term: only when we're
            // already past the first char and see " -" — hyphens inside stay.
            i += 1;
        }
        let raw: String = chars[start..i].iter().collect();
        if raw.is_empty() {
            continue;
        }

        // field:"phrase" — if word ends with `:` and next is quote, fold field.
        if let Some((field_name, rest)) = raw.split_once(':') {
            if rest.is_empty() && i < chars.len() && chars[i] == '"' {
                if let Some(field) = parse_field(field_name) {
                    i += 1; // "
                    let mut s = String::new();
                    while i < chars.len() && chars[i] != '"' {
                        s.push(chars[i]);
                        i += 1;
                    }
                    if i < chars.len() {
                        i += 1;
                    }
                    out.push(Tok::Term {
                        field: Some(field),
                        text: s,
                        phrase: true,
                        explicit_prefix: false,
                    });
                    continue;
                }
            }
            if !rest.is_empty() {
                if let Some(field) = parse_field(field_name) {
                    let (text, explicit_prefix) = strip_star(rest);
                    if text.is_empty() {
                        continue;
                    }
                    // Operators after field: are still terms (e.g. title:OR is a term).
                    out.push(Tok::Term {
                        field: Some(field),
                        text: text.to_string(),
                        phrase: false,
                        explicit_prefix,
                    });
                    continue;
                }
            }
        }

        let upper = raw.to_ascii_uppercase();
        match upper.as_str() {
            "OR" => out.push(Tok::Or),
            "AND" => out.push(Tok::And),
            "NOT" => out.push(Tok::Not),
            _ => {
                let (text, explicit_prefix) = strip_star(&raw);
                if text.is_empty() {
                    continue;
                }
                out.push(Tok::Term {
                    field: None,
                    text: text.to_string(),
                    phrase: false,
                    explicit_prefix,
                });
            }
        }
    }
    out
}

fn parse_field(name: &str) -> Option<Field> {
    match name.to_ascii_lowercase().as_str() {
        "title" => Some(Field::Title),
        "body" => Some(Field::Body),
        "feed" => Some(Field::Feed),
        _ => None,
    }
}

fn strip_star(s: &str) -> (&str, bool) {
    if let Some(rest) = s.strip_suffix('*') {
        (rest, true)
    } else {
        (s, false)
    }
}

// ── AST ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone)]
enum Node {
    And(Vec<Node>),
    Or(Vec<Node>),
    Not(Box<Node>),
    /// One or more FTS terms (punctuation-split parts of an unquoted word),
    /// AND-joined, optionally under a column filter.
    Terms {
        field: Option<Field>,
        parts: Vec<TermPart>,
    },
    Feed(String),
}

#[derive(Debug, Clone)]
struct TermPart {
    text: String,
    phrase: bool,
    prefix: bool,
}

struct Parser<'a> {
    tokens: &'a [Tok],
    pos: usize,
    mode: SearchMode,
    ok: bool,
}

impl Parser<'_> {
    fn peek(&self) -> Option<&Tok> {
        self.tokens.get(self.pos)
    }

    fn bump(&mut self) -> Option<&Tok> {
        let t = self.tokens.get(self.pos)?;
        self.pos += 1;
        Some(t)
    }

    fn parse_expr(&mut self) -> Option<Node> {
        self.parse_or()
    }

    fn parse_or(&mut self) -> Option<Node> {
        let mut left = self.parse_and()?;
        while matches!(self.peek(), Some(Tok::Or)) {
            self.bump();
            let right = self.parse_and()?;
            left = match left {
                Node::Or(mut v) => {
                    v.push(right);
                    Node::Or(v)
                }
                other => Node::Or(vec![other, right]),
            };
        }
        Some(left)
    }

    fn parse_and(&mut self) -> Option<Node> {
        // Explicit `AND` always means AND. Bare juxtaposition uses `mode`
        // (strict → AND, recall → OR).
        let mut acc: Option<Node> = None;
        let mut force_and = false;
        loop {
            while matches!(self.peek(), Some(Tok::And)) {
                self.bump();
                force_and = true;
            }
            match self.peek() {
                None | Some(Tok::Or) | Some(Tok::RParen) => break,
                Some(Tok::Term { .. }) | Some(Tok::Not) | Some(Tok::LParen) => {}
                Some(Tok::And) => unreachable!(),
            }
            let Some(n) = self.parse_unary() else {
                break;
            };
            acc = Some(match acc {
                None => n,
                Some(prev) => {
                    let join_and = force_and || matches!(self.mode, SearchMode::Strict);
                    if join_and {
                        merge_and(prev, n)
                    } else {
                        merge_or(prev, n)
                    }
                }
            });
            force_and = false;
        }
        acc
    }

    fn parse_unary(&mut self) -> Option<Node> {
        if matches!(self.peek(), Some(Tok::Not)) {
            self.bump();
            let inner = self.parse_unary()?;
            return Some(Node::Not(Box::new(inner)));
        }
        self.parse_primary()
    }

    fn parse_primary(&mut self) -> Option<Node> {
        match self.peek()?.clone() {
            Tok::LParen => {
                self.bump();
                let inner = self.parse_expr()?;
                if !matches!(self.peek(), Some(Tok::RParen)) {
                    self.ok = false;
                    return None;
                }
                self.bump();
                Some(inner)
            }
            Tok::Term {
                field,
                text,
                phrase,
                explicit_prefix,
            } => {
                self.bump();
                term_node(field, text, phrase, explicit_prefix)
            }
            Tok::Or | Tok::And | Tok::Not | Tok::RParen => None,
        }
    }
}

fn merge_and(a: Node, b: Node) -> Node {
    match (a, b) {
        (Node::And(mut xs), Node::And(ys)) => {
            xs.extend(ys);
            Node::And(xs)
        }
        (Node::And(mut xs), other) => {
            xs.push(other);
            Node::And(xs)
        }
        (other, Node::And(mut xs)) => {
            xs.insert(0, other);
            Node::And(xs)
        }
        (a, b) => Node::And(vec![a, b]),
    }
}

fn merge_or(a: Node, b: Node) -> Node {
    match (a, b) {
        (Node::Or(mut xs), Node::Or(ys)) => {
            xs.extend(ys);
            Node::Or(xs)
        }
        (Node::Or(mut xs), other) => {
            xs.push(other);
            Node::Or(xs)
        }
        (other, Node::Or(mut xs)) => {
            xs.insert(0, other);
            Node::Or(xs)
        }
        (a, b) => Node::Or(vec![a, b]),
    }
}

/// Short ASCII tokens (≤3 chars) must be whole-token matches — prefix `"ai"*`
/// would also hit `against` / `aid`. CJK and longer Latin keep default prefix.
fn is_short_latin(text: &str) -> bool {
    let mut n = 0;
    for c in text.chars() {
        if !c.is_ascii_alphanumeric() {
            return false;
        }
        n += 1;
        if n > 3 {
            return false;
        }
    }
    n > 0 && n <= 3
}

/// Whether an unquoted bare term should receive the automatic trailing `*`.
fn wants_auto_prefix(text: &str) -> bool {
    !is_short_latin(text)
}

fn term_node(
    field: Option<Field>,
    text: String,
    phrase: bool,
    explicit_prefix: bool,
) -> Option<Node> {
    if matches!(field, Some(Field::Feed)) {
        let name = text.trim();
        if name.is_empty() {
            return None;
        }
        return Some(Node::Feed(name.to_string()));
    }
    if phrase {
        if text.trim().is_empty() {
            return None;
        }
        return Some(Node::Terms {
            field,
            parts: vec![TermPart {
                text,
                phrase: true,
                prefix: false,
            }],
        });
    }
    // Split unquoted text on non-alphanumeric (unicode61 alignment).
    // Default prefix on unquoted parts, except short Latin (≤3) — whole token.
    // Explicit trailing `*` still forces prefix even for short Latin.
    let parts: Vec<TermPart> = text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| TermPart {
            text: t.to_string(),
            phrase: false,
            prefix: explicit_prefix || wants_auto_prefix(t),
        })
        .collect();
    if parts.is_empty() {
        return None;
    }
    Some(Node::Terms { field, parts })
}

fn collect_feeds(node: &Node, out: &mut Vec<String>) {
    match node {
        Node::And(xs) | Node::Or(xs) => {
            for x in xs {
                collect_feeds(x, out);
            }
        }
        Node::Not(x) => collect_feeds(x, out),
        Node::Feed(name) => out.push(name.clone()),
        Node::Terms { .. } => {}
    }
}

/// Compile AST to FTS5, dropping `feed:` nodes (handled via SQL).
///
/// FTS5 `NOT` is binary (`a NOT b`), so bare unary NOT is dropped / match-nothing.
fn compile_node(node: &Node, mode: SearchMode, dict: Option<&WordCloudDict>) -> Option<String> {
    match node {
        Node::Feed(_) => None,
        Node::Terms { field, parts } => compile_terms(field, parts, dict),
        Node::Not(inner) => {
            // Unary NOT alone is not valid FTS5; only meaningful inside And.
            let _ = (inner, mode);
            None
        }
        Node::And(xs) => {
            let mut positives: Vec<String> = Vec::new();
            let mut negatives: Vec<String> = Vec::new();
            // Collapse duplicate synonym groups under AND (Trump + 特朗普 → one).
            let mut seen_entities: Vec<String> = Vec::new();
            for n in xs {
                match n {
                    Node::Not(inner) => {
                        if let Some(s) = compile_node(inner, mode, dict) {
                            negatives.push(s);
                        }
                    }
                    other => {
                        if let Some(key) = terms_synonym_entity(other, dict) {
                            if seen_entities.iter().any(|e| e == &key) {
                                continue;
                            }
                            seen_entities.push(key);
                        }
                        if let Some(s) = compile_node(other, mode, dict) {
                            positives.push(s);
                        }
                    }
                }
            }
            if positives.is_empty() {
                return None;
            }
            // FTS5 rejects implicit AND after a parenthesized OR group
            // (`(a OR b) c` → syntax error). Always emit explicit AND.
            let mut s = positives.join(" AND ");
            for neg in negatives {
                s = format!("{s} NOT {neg}");
            }
            Some(s)
        }
        Node::Or(xs) => {
            let mut parts: Vec<String> = Vec::new();
            let mut seen_entities: Vec<String> = Vec::new();
            for n in xs {
                match n {
                    // Skip bare NOT inside OR (invalid FTS5).
                    Node::Not(_) => {}
                    other => {
                        if let Some(eid) = terms_synonym_entity(other, dict) {
                            if seen_entities.iter().any(|e| e == &eid) {
                                continue;
                            }
                            seen_entities.push(eid);
                        }
                        if let Some(s) = compile_node(other, mode, dict) {
                            parts.push(s);
                        }
                    }
                }
            }
            if parts.is_empty() {
                None
            } else if parts.len() == 1 {
                Some(parts.into_iter().next().unwrap())
            } else {
                Some(format!("({})", parts.join(" OR ")))
            }
        }
    }
}

/// If `node` is a single bare (non-phrase) term that expands to one entity, return its id.
fn terms_synonym_entity(node: &Node, dict: Option<&WordCloudDict>) -> Option<String> {
    let dict = dict?;
    let Node::Terms { parts, .. } = node else {
        return None;
    };
    if parts.len() != 1 || parts[0].phrase {
        return None;
    }
    dict.lookup_synonym_group(&parts[0].text)
        .map(|g| g.id)
}

fn compile_terms(
    field: &Option<Field>,
    parts: &[TermPart],
    dict: Option<&WordCloudDict>,
) -> Option<String> {
    let col = match field {
        Some(Field::Title) => Some("title"),
        Some(Field::Body) => Some("body"),
        Some(Field::Feed) | None => None,
    };
    let pieces: Vec<String> = parts
        .iter()
        .filter_map(|p| compile_term_part(col, p, dict))
        .collect();
    if pieces.is_empty() {
        None
    } else if pieces.len() == 1 {
        Some(pieces.into_iter().next().unwrap())
    } else {
        // Parts of one tokenized word always AND together.
        Some(pieces.join(" AND "))
    }
}

fn compile_term_part(
    column: Option<&str>,
    part: &TermPart,
    dict: Option<&WordCloudDict>,
) -> Option<String> {
    if part.phrase {
        return Some(format_term(column, &part.text, true, false));
    }
    if let Some(dict) = dict {
        if let Some(group) = dict.lookup_synonym_group(&part.text) {
            // Entity / word-cloud terms: whole-token match (no auto-prefix), so
            // short aliases like `AI`/`ai` never become `ai*` → against/aid.
            return Some(format_synonym_group(column, &group));
        }
    }
    Some(format_term(
        column,
        &part.text,
        false,
        part.prefix,
    ))
}

/// OR of all aliases in a synonym group as whole tokens (no trailing `*`).
/// Multi-word aliases become exact phrases.
fn format_synonym_group(column: Option<&str>, group: &SynonymGroup) -> String {
    let pieces: Vec<String> = group
        .aliases
        .iter()
        .map(|alias| {
            if alias.split_whitespace().count() > 1 {
                format_term(column, alias, true, false)
            } else {
                format_term(column, alias, false, false)
            }
        })
        .collect();
    if pieces.is_empty() {
        format_term(column, &group.canonical, false, false)
    } else if pieces.len() == 1 {
        pieces.into_iter().next().unwrap()
    } else {
        format!("({})", pieces.join(" OR "))
    }
}

fn format_term(column: Option<&str>, text: &str, phrase: bool, prefix: bool) -> String {
    let escaped = text.replace('"', "\"\"");
    let body = if phrase {
        format!("\"{escaped}\"")
    } else if prefix {
        format!("\"{escaped}\"*")
    } else {
        format!("\"{escaped}\"")
    };
    match column {
        Some(col) => format!("{col}:{body}"),
        None => body,
    }
}

/// Extract plain terms from a query for UI hit highlighting (operators ignored).
pub fn highlight_terms(input: &str) -> Vec<String> {
    highlight_terms_with_dict(input, None)
}

/// Like [`highlight_terms`], expanding needles to synonym aliases when `dict` is set.
pub fn highlight_terms_with_dict(input: &str, dict: Option<&WordCloudDict>) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let push = |s: String, out: &mut Vec<String>, seen: &mut std::collections::HashSet<String>| {
        let key = s.to_lowercase();
        if seen.insert(key) {
            out.push(s);
        }
    };
    for t in tokenize(input) {
        match t {
            Tok::Term {
                field: Some(Field::Feed),
                ..
            } => {}
            Tok::Term { text, phrase, .. } => {
                if phrase {
                    if !text.is_empty() {
                        push(text, &mut out, &mut seen);
                    }
                } else {
                    for part in text.split(|c: char| !c.is_alphanumeric()) {
                        if part.is_empty() {
                            continue;
                        }
                        if let Some(dict) = dict {
                            if let Some(group) = dict.lookup_synonym_group(part) {
                                for a in group.aliases {
                                    push(a, &mut out, &mut seen);
                                }
                                continue;
                            }
                        }
                        push(part.to_string(), &mut out, &mut seen);
                    }
                }
            }
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wordcloud_dict::{EntitiesFile, WordCloudDict, WordCloudEntity};
    use std::path::PathBuf;

    fn strict(q: &str) -> String {
        fts_match_expr(q, SearchMode::Strict)
    }

    fn recall(q: &str) -> String {
        fts_match_expr(q, SearchMode::Recall)
    }

    fn test_dict() -> WordCloudDict {
        let mut dict = WordCloudDict::empty(PathBuf::from("/tmp"));
        dict.apply_entities(EntitiesFile {
            version: 1,
            entities: vec![
                WordCloudEntity {
                    id: "person.trump".into(),
                    canonical: "Trump".into(),
                    group: "person".into(),
                    aliases: vec![
                        "trump".into(),
                        "donald trump".into(),
                        "特朗普".into(),
                        "川普".into(),
                    ],
                },
                WordCloudEntity {
                    id: "country.china".into(),
                    canonical: "China".into(),
                    group: "country".into(),
                    aliases: vec!["china".into(), "中国".into()],
                },
                WordCloudEntity {
                    id: "person.biden".into(),
                    canonical: "Biden".into(),
                    group: "person".into(),
                    aliases: vec!["biden".into(), "拜登".into()],
                },
            ],
        });
        dict
    }

    fn strict_syn(q: &str) -> String {
        let dict = test_dict();
        fts_match_expr_with_dict(q, SearchMode::Strict, Some(&dict))
    }

    #[test]
    fn and_default_strict() {
        assert_eq!(strict("Trump china"), "\"Trump\"* AND \"china\"*");
    }

    #[test]
    fn or_default_recall() {
        assert_eq!(
            recall("Trump china"),
            "(\"Trump\"* OR \"china\"*)"
        );
    }

    #[test]
    fn phrase() {
        assert_eq!(strict("\"Trump China\""), "\"Trump China\"");
        assert_eq!(strict("\"interest rate\""), "\"interest rate\"");
    }

    #[test]
    fn explicit_or() {
        assert_eq!(strict("Trump OR Biden"), "(\"Trump\"* OR \"Biden\"*)");
    }

    #[test]
    fn not_minus_and_keyword() {
        assert_eq!(strict("Trump -china"), "\"Trump\"* NOT \"china\"*");
        assert_eq!(strict("Trump NOT china"), "\"Trump\"* NOT \"china\"*");
        assert_eq!(strict("Trump -tariff"), "\"Trump\"* NOT \"tariff\"*");
    }

    #[test]
    fn grouping() {
        assert_eq!(
            strict("(Trump OR Biden) china"),
            "(\"Trump\"* OR \"Biden\"*) AND \"china\"*"
        );
    }

    #[test]
    fn title_and_body_fields() {
        assert_eq!(strict("title:Trump"), "title:\"Trump\"*");
        assert_eq!(strict("body:sanctions"), "body:\"sanctions\"*");
        assert_eq!(
            strict("title:Trump body:tariff"),
            "title:\"Trump\"* AND body:\"tariff\"*"
        );
    }

    #[test]
    fn title_phrase() {
        assert_eq!(
            strict("title:\"Federal Reserve\""),
            "title:\"Federal Reserve\""
        );
    }

    #[test]
    fn feed_filter_extracted() {
        let c = compile_search("feed:Reuters Trump", SearchMode::Strict);
        assert_eq!(c.match_expr.as_deref(), Some("\"Trump\"*"));
        assert_eq!(c.feed_prefixes, vec!["Reuters"]);
    }

    #[test]
    fn feed_only() {
        let c = compile_search("feed:Nikkei", SearchMode::Strict);
        assert!(c.match_expr.is_none());
        assert_eq!(c.feed_prefixes, vec!["Nikkei"]);
    }

    #[test]
    fn compound() {
        assert_eq!(
            strict("title:\"Federal Reserve\" (inflation OR rates) -opinion"),
            "title:\"Federal Reserve\" AND (\"inflation\"* OR \"rates\"*) NOT \"opinion\"*"
        );
    }

    #[test]
    fn empty_and_punct() {
        assert_eq!(strict(""), "\"\"");
        assert_eq!(strict("   "), "\"\"");
        assert_eq!(strict("??? !!!"), "\"\"");
        assert!(compile_search("???", SearchMode::Strict).is_empty());
    }

    #[test]
    fn explicit_prefix_and_hyphen_split() {
        assert_eq!(strict("chin*"), "\"chin\"*");
        assert_eq!(strict("rust-lang"), "\"rust\"* AND \"lang\"*");
    }

    #[test]
    fn explicit_and_keyword() {
        assert_eq!(strict("Trump AND china"), "\"Trump\"* AND \"china\"*");
    }

    #[test]
    fn unbalanced_parens_match_nothing() {
        assert_eq!(strict("(Trump OR Biden"), "\"\"");
    }

    #[test]
    fn synonym_single_term_expands() {
        let expr = strict_syn("特朗普");
        // Entity expansion is whole-token (no trailing *).
        assert!(expr.contains("\"特朗普\""), "{expr}");
        assert!(
            expr.contains("\"Trump\"") || expr.contains("\"trump\""),
            "{expr}"
        );
        assert!(!expr.contains("\"特朗普\"*"), "{expr}");
        assert!(expr.starts_with('(') && expr.ends_with(')'), "{expr}");
        assert!(expr.contains(" OR "), "{expr}");
    }

    #[test]
    fn synonym_same_entity_collapses_under_and() {
        let a = strict_syn("Trump");
        let b = strict_syn("Trump 特朗普");
        assert_eq!(a, b, "same entity must collapse to one OR-group");
        assert!(!b.contains(" AND "), "{b}");
    }

    #[test]
    fn synonym_different_entities_stay_and() {
        let expr = strict_syn("Trump china");
        assert!(expr.contains(" AND "), "{expr}");
        // Two groups, not a false merge into one.
        let parts: Vec<_> = expr.split(" AND ").collect();
        assert_eq!(parts.len(), 2, "{expr}");
    }

    #[test]
    fn synonym_no_false_merge_across_people() {
        let expr = strict_syn("Trump Biden");
        assert!(expr.contains(" AND "), "{expr}");
        let parts: Vec<_> = expr.split(" AND ").collect();
        assert_eq!(parts.len(), 2, "{expr}");
    }

    #[test]
    fn synonym_phrase_not_expanded() {
        assert_eq!(strict_syn("\"Trump 特朗普\""), "\"Trump 特朗普\"");
    }

    #[test]
    fn synonym_title_field_expands() {
        let expr = strict_syn("title:特朗普");
        assert!(expr.contains("title:"), "{expr}");
        assert!(expr.contains("特朗普"), "{expr}");
        assert!(expr.contains(" OR "), "{expr}");
    }

    #[test]
    fn synonym_explicit_or_still_works() {
        let expr = strict_syn("Trump OR Biden");
        assert!(expr.contains(" OR "), "{expr}");
        // Top-level OR of two expanded groups.
        assert!(expr.starts_with('('), "{expr}");
    }

    #[test]
    fn synonym_empty_still_match_nothing() {
        let dict = test_dict();
        assert_eq!(
            fts_match_expr_with_dict("???", SearchMode::Strict, Some(&dict)),
            "\"\""
        );
    }

    #[test]
    fn synonym_no_extra_aliases_compiles_whole_token() {
        let mut dict = WordCloudDict::empty(PathBuf::from("/tmp"));
        dict.apply_entities(EntitiesFile {
            version: 1,
            entities: vec![WordCloudEntity {
                id: "general.solo".into(),
                canonical: "Solo".into(),
                group: "general".into(),
                aliases: vec![],
            }],
        });
        assert_eq!(
            fts_match_expr_with_dict("Solo", SearchMode::Strict, Some(&dict)),
            "\"Solo\""
        );
    }

    #[test]
    fn highlight_expands_aliases() {
        let dict = test_dict();
        let terms = highlight_terms_with_dict("Trump", Some(&dict));
        assert!(terms.iter().any(|t| t == "特朗普"), "{terms:?}");
        assert!(terms.iter().any(|t| t.eq_ignore_ascii_case("trump")), "{terms:?}");
    }

    #[test]
    fn short_latin_no_auto_prefix() {
        // Avoid `"ai"*` matching against / aid; FTS5 still casefolds.
        assert_eq!(strict("AI"), "\"AI\"");
        assert_eq!(strict("ai"), "\"ai\"");
        assert_eq!(strict("US"), "\"US\"");
        // Explicit trailing * still forces prefix.
        assert_eq!(strict("ai*"), "\"ai\"*");
        // Longer bare terms keep default prefix.
        assert_eq!(strict("china"), "\"china\"*");
    }

    #[test]
    fn synonym_short_latin_ai_whole_token() {
        let mut dict = WordCloudDict::empty(PathBuf::from("/tmp"));
        dict.apply_entities(EntitiesFile {
            version: 1,
            entities: vec![WordCloudEntity {
                id: "topic.ai".into(),
                canonical: "AI".into(),
                group: "topic".into(),
                aliases: vec!["ai".into(), "artificial intelligence".into()],
            }],
        });
        let expr = fts_match_expr_with_dict("ai", SearchMode::Strict, Some(&dict));
        assert!(expr.contains("\"AI\"") || expr.contains("\"ai\""), "{expr}");
        assert!(!expr.contains("\"ai\"*"), "{expr}");
        assert!(!expr.contains("\"AI\"*"), "{expr}");
        assert!(expr.contains("\"artificial intelligence\""), "{expr}");
    }
}

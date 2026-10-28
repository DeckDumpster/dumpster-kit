//! Search plumbing: a query language compiled to a parameterised SQL `WHERE` clause.
//!
//! Grammar (keywords case-insensitive):
//!
//! ```text
//! query  := or
//! or     := and ("or" and)*
//! and    := unary (["and"] unary)*        -- juxtaposition is "and"
//! unary  := ("not" | "-") unary | atom
//! atom   := "(" query ")" | clause | WORD | "quoted phrase"
//! clause := TERM OP VALUE                 -- OP is : = != < <= > >=
//! ```
//!
//! A bare word or quoted phrase matches the vocabulary's name column. Text terms take `:`
//! (contains), `=` and `!=` (exact); numeric terms take every comparison, `:` meaning `=`.
//! Every user-supplied value is a bound parameter; column names come only from the vocabulary
//! and are validated as identifiers when it is built.

use rusqlite::ToSql;
use rusqlite::types::ToSqlOutput;

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum SearchError {
    #[error("{0}")]
    Vocabulary(String),
    #[error("{message} (at offset {offset})")]
    Parse { message: String, offset: usize },
    #[error("unknown term `{term}` (at offset {offset}); known terms: {known}")]
    UnknownTerm {
        term: String,
        offset: usize,
        known: String,
    },
}

fn perr(message: impl Into<String>, offset: usize) -> SearchError {
    SearchError::Parse {
        message: message.into(),
        offset,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TermKind {
    Text,
    Number,
}

/// One entry of a game's search vocabulary: data, not code.
#[derive(Debug, Clone)]
pub struct SearchTerm {
    pub canonical: String,
    pub aliases: Vec<String>,
    pub kind: TermKind,
    /// The SQL column (optionally `table.column`) the term reads.
    pub column: String,
    pub help: String,
}

impl SearchTerm {
    pub fn new(canonical: &str, kind: TermKind, column: &str, help: &str) -> Self {
        SearchTerm {
            canonical: canonical.into(),
            aliases: Vec::new(),
            kind,
            column: column.into(),
            help: help.into(),
        }
    }

    pub fn alias(mut self, alias: &str) -> Self {
        self.aliases.push(alias.into());
        self
    }
}

#[derive(Debug, Clone)]
pub struct Vocabulary {
    name_column: String,
    terms: Vec<SearchTerm>,
}

fn is_ident(s: &str) -> bool {
    !s.is_empty()
        && s.split('.').all(|p| {
            let mut c = p.chars();
            c.next()
                .is_some_and(|f| f.is_ascii_alphabetic() || f == '_')
                && c.all(|c| c.is_ascii_alphanumeric() || c == '_')
        })
}

impl Vocabulary {
    /// `name_column` is what bare words and phrases search.
    pub fn new(name_column: &str, terms: Vec<SearchTerm>) -> Result<Self, SearchError> {
        let bad =
            |what: &str, v: &str| SearchError::Vocabulary(format!("{what} `{v}` is not valid"));
        if !is_ident(name_column) {
            return Err(bad("name column", name_column));
        }
        let mut seen: Vec<String> = Vec::new();
        for t in &terms {
            if !is_ident(&t.column) {
                return Err(bad("column", &t.column));
            }
            for n in std::iter::once(&t.canonical).chain(&t.aliases) {
                let ok = !n.is_empty()
                    && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                    && !["and", "or", "not"].contains(&n.to_ascii_lowercase().as_str());
                if !ok {
                    return Err(bad("term name", n));
                }
                let l = n.to_ascii_lowercase();
                if seen.contains(&l) {
                    return Err(SearchError::Vocabulary(format!(
                        "term `{n}` is declared twice"
                    )));
                }
                seen.push(l);
            }
        }
        Ok(Vocabulary {
            name_column: name_column.into(),
            terms,
        })
    }

    pub fn terms(&self) -> &[SearchTerm] {
        &self.terms
    }

    fn find(&self, name: &str) -> Option<&SearchTerm> {
        self.terms.iter().find(|t| {
            t.canonical.eq_ignore_ascii_case(name)
                || t.aliases.iter().any(|a| a.eq_ignore_ascii_case(name))
        })
    }

    fn known(&self) -> String {
        let mut names: Vec<&str> = self
            .terms
            .iter()
            .flat_map(|t| {
                std::iter::once(t.canonical.as_str()).chain(t.aliases.iter().map(String::as_str))
            })
            .collect();
        names.sort_unstable();
        names.join(", ")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Op {
    Contains,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

impl Op {
    fn sql(self) -> &'static str {
        match self {
            Op::Contains | Op::Eq => "=",
            Op::Ne => "<>",
            Op::Lt => "<",
            Op::Le => "<=",
            Op::Gt => ">",
            Op::Ge => ">=",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Name(String),
    Clause {
        term: String,
        op: Op,
        value: String,
        offset: usize,
    },
    And(Vec<Expr>),
    Or(Vec<Expr>),
    Not(Box<Expr>),
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    LParen,
    RParen,
    Word(String),
    Quoted(String),
    Op(Op),
}

fn lex(src: &str) -> Result<Vec<(Tok, usize)>, SearchError> {
    let chars: Vec<(usize, char)> = src.char_indices().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let (off, c) = chars[i];
        let next = chars.get(i + 1).map(|p| p.1);
        match c {
            c if c.is_whitespace() => i += 1,
            '(' => {
                out.push((Tok::LParen, off));
                i += 1;
            }
            ')' => {
                out.push((Tok::RParen, off));
                i += 1;
            }
            '"' => {
                let mut s = String::new();
                i += 1;
                loop {
                    match chars.get(i) {
                        None => return Err(perr("unterminated quote", off)),
                        Some((_, '"')) => break,
                        Some((_, ch)) => s.push(*ch),
                    }
                    i += 1;
                }
                i += 1;
                out.push((Tok::Quoted(s), off));
            }
            ':' => {
                out.push((Tok::Op(Op::Contains), off));
                i += 1;
            }
            '=' => {
                out.push((Tok::Op(Op::Eq), off));
                i += 1;
            }
            '!' if next == Some('=') => {
                out.push((Tok::Op(Op::Ne), off));
                i += 2;
            }
            '<' | '>' => {
                let (op, n) = match (c, next == Some('=')) {
                    ('<', true) => (Op::Le, 2),
                    ('<', false) => (Op::Lt, 1),
                    (_, true) => (Op::Ge, 2),
                    _ => (Op::Gt, 1),
                };
                out.push((Tok::Op(op), off));
                i += n;
            }
            _ => {
                let mut s = String::new();
                while let Some(&(_, ch)) = chars.get(i) {
                    let stop = ch.is_whitespace()
                        || "():=<>\"".contains(ch)
                        || (ch == '!' && chars.get(i + 1).map(|p| p.1) == Some('='));
                    if stop {
                        break;
                    }
                    s.push(ch);
                    i += 1;
                }
                out.push((Tok::Word(s), off));
            }
        }
    }
    Ok(out)
}

struct Parser {
    toks: Vec<(Tok, usize)>,
    pos: usize,
    end: usize,
}

fn keyword(t: Option<&(Tok, usize)>, kw: &str) -> bool {
    matches!(t, Some((Tok::Word(w), _)) if w.eq_ignore_ascii_case(kw))
}

impl Parser {
    fn peek(&self) -> Option<&(Tok, usize)> {
        self.toks.get(self.pos)
    }

    fn off(&self) -> usize {
        self.peek().map_or(self.end, |t| t.1)
    }

    fn or(&mut self) -> Result<Expr, SearchError> {
        let mut parts = vec![self.and()?];
        while keyword(self.peek(), "or") {
            self.pos += 1;
            parts.push(self.and()?);
        }
        Ok(if parts.len() == 1 {
            parts.remove(0)
        } else {
            Expr::Or(parts)
        })
    }

    fn and(&mut self) -> Result<Expr, SearchError> {
        let mut parts = vec![self.unary()?];
        loop {
            if keyword(self.peek(), "and") {
                self.pos += 1;
            } else if matches!(self.peek(), None | Some((Tok::RParen, _)))
                || keyword(self.peek(), "or")
            {
                break;
            }
            parts.push(self.unary()?);
        }
        Ok(if parts.len() == 1 {
            parts.remove(0)
        } else {
            Expr::And(parts)
        })
    }

    fn unary(&mut self) -> Result<Expr, SearchError> {
        if keyword(self.peek(), "not") {
            self.pos += 1;
            return Ok(Expr::Not(Box::new(self.unary()?)));
        }
        if let Some((Tok::Word(w), off)) = self.peek()
            && w.len() > 1
            && w.starts_with('-')
        {
            // `-word` / `-term:value`: strip the dash from the token and negate the rest.
            let (w, off) = (w[1..].to_string(), *off + 1);
            self.toks[self.pos] = (Tok::Word(w), off);
            return Ok(Expr::Not(Box::new(self.unary()?)));
        }
        self.atom()
    }

    fn atom(&mut self) -> Result<Expr, SearchError> {
        let off = self.off();
        match self.peek().cloned() {
            None => Err(perr("expected a search term, found end of query", off)),
            Some((Tok::LParen, _)) => {
                self.pos += 1;
                let e = self.or()?;
                match self.peek() {
                    Some((Tok::RParen, _)) => {
                        self.pos += 1;
                        Ok(e)
                    }
                    _ => Err(perr("missing closing `)`", self.off())),
                }
            }
            Some((Tok::RParen, o)) => Err(perr("unexpected `)`", o)),
            Some((Tok::Op(_), o)) => Err(perr("operator with no term before it", o)),
            Some((Tok::Quoted(s), _)) => {
                self.pos += 1;
                Ok(Expr::Name(s))
            }
            Some((Tok::Word(w), o)) => {
                self.pos += 1;
                if let Some((Tok::Op(op), _)) = self.peek().cloned() {
                    self.pos += 1;
                    return match self.peek().cloned() {
                        Some((Tok::Word(v) | Tok::Quoted(v), _)) => {
                            self.pos += 1;
                            Ok(Expr::Clause {
                                term: w,
                                op,
                                value: v,
                                offset: o,
                            })
                        }
                        _ => Err(perr(
                            format!("`{w}` needs a value after the operator"),
                            self.off(),
                        )),
                    };
                }
                Ok(Expr::Name(w))
            }
        }
    }
}

/// Parse a query without checking terms against a vocabulary.
pub fn parse(src: &str) -> Result<Expr, SearchError> {
    let mut p = Parser {
        toks: lex(src)?,
        pos: 0,
        end: src.len(),
    };
    if p.toks.is_empty() {
        return Err(perr("empty query", 0));
    }
    let e = p.or()?;
    if let Some((_, o)) = p.peek() {
        return Err(perr("unexpected token", *o));
    }
    Ok(e)
}

#[derive(Debug, Clone, PartialEq)]
pub enum Param {
    Text(String),
    Integer(i64),
    Real(f64),
}

impl ToSql for Param {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        match self {
            Param::Text(s) => s.to_sql(),
            Param::Integer(i) => i.to_sql(),
            Param::Real(r) => r.to_sql(),
        }
    }
}

/// A compiled query: a `WHERE` condition with `?` placeholders and their values in order.
#[derive(Debug, Clone, PartialEq)]
pub struct Compiled {
    pub sql: String,
    pub params: Vec<Param>,
}

impl Compiled {
    pub fn params(&self) -> Vec<&dyn ToSql> {
        self.params.iter().map(|p| p as &dyn ToSql).collect()
    }
}

fn like_pattern(v: &str) -> String {
    let mut s = String::from("%");
    for c in v.chars() {
        if matches!(c, '%' | '_' | '\\') {
            s.push('\\');
        }
        s.push(c);
    }
    s.push('%');
    s
}

fn gen_sql(e: &Expr, v: &Vocabulary, params: &mut Vec<Param>) -> Result<String, SearchError> {
    Ok(match e {
        Expr::Name(s) => {
            params.push(Param::Text(like_pattern(s)));
            format!("{} LIKE ? ESCAPE '\\'", v.name_column)
        }
        Expr::Not(inner) => format!("NOT ({})", gen_sql(inner, v, params)?),
        Expr::And(xs) | Expr::Or(xs) => {
            let join = if matches!(e, Expr::And(_)) {
                " AND "
            } else {
                " OR "
            };
            let parts = xs
                .iter()
                .map(|x| gen_sql(x, v, params))
                .collect::<Result<Vec<_>, _>>()?;
            format!("({})", parts.join(join))
        }
        Expr::Clause {
            term,
            op,
            value,
            offset,
        } => {
            let t = v.find(term).ok_or_else(|| SearchError::UnknownTerm {
                term: term.clone(),
                offset: *offset,
                known: v.known(),
            })?;
            match t.kind {
                TermKind::Number => {
                    let p = if let Ok(i) = value.parse::<i64>() {
                        Param::Integer(i)
                    } else if let Ok(r) = value.parse::<f64>() {
                        Param::Real(r)
                    } else {
                        return Err(perr(
                            format!("`{}` takes a number, not `{value}`", t.canonical),
                            *offset,
                        ));
                    };
                    params.push(p);
                    format!("{} {} ?", t.column, op.sql())
                }
                TermKind::Text => match op {
                    Op::Contains => {
                        params.push(Param::Text(like_pattern(value)));
                        format!("{} LIKE ? ESCAPE '\\'", t.column)
                    }
                    Op::Eq | Op::Ne => {
                        params.push(Param::Text(value.clone()));
                        format!("{} {} ? COLLATE NOCASE", t.column, op.sql())
                    }
                    _ => {
                        return Err(perr(
                            format!("`{}` is a text term and takes only : = !=", t.canonical),
                            *offset,
                        ));
                    }
                },
            }
        }
    })
}

/// Parse `src` and compile it over `vocab`. An unknown term is an error naming the known terms.
pub fn compile(src: &str, vocab: &Vocabulary) -> Result<Compiled, SearchError> {
    let expr = parse(src)?;
    let mut params = Vec::new();
    let sql = gen_sql(&expr, vocab, &mut params)?;
    Ok(Compiled { sql, params })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    /// An invented game: no real game's fields.
    fn vocab() -> Vocabulary {
        Vocabulary::new(
            "name",
            vec![
                SearchTerm::new("color", TermKind::Text, "color", "the glimmer color").alias("c"),
                SearchTerm::new("weight", TermKind::Number, "weight", "weight in grams").alias("w"),
                SearchTerm::new("shape", TermKind::Text, "g.shape", "shape"),
            ],
        )
        .unwrap()
    }

    fn run(q: &str) -> Vec<String> {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch(
            "CREATE TABLE g(id INTEGER PRIMARY KEY, name TEXT, color TEXT, weight REAL, shape TEXT);
             INSERT INTO g(name,color,weight,shape) VALUES
              ('Blue Gizmo','blue',3,'cube'),('Red Gizmo','red',5,'cube'),
              ('Red Wobble','red',1.5,'orb'),('100% Pure','green',2,'orb');",
        )
        .unwrap();
        let c = compile(q, &vocab()).unwrap();
        let sql = format!(
            "SELECT name FROM g WHERE {} ORDER BY id",
            c.sql.replace("g.shape", "shape")
        );
        let mut st = db.prepare(&sql).unwrap();
        st.query_map(c.params().as_slice(), |r| r.get(0))
            .unwrap()
            .map(Result::unwrap)
            .collect()
    }

    #[test]
    fn name_phrase_and_terms() {
        assert_eq!(run("gizmo"), ["Blue Gizmo", "Red Gizmo"]);
        assert_eq!(run("\"red gizmo\""), ["Red Gizmo"]);
        assert_eq!(run("color:red weight>=2"), ["Red Gizmo"]);
        assert_eq!(run("c=BLUE"), ["Blue Gizmo"]);
        assert_eq!(run("w<3 and not c:green"), ["Red Wobble"]);
    }

    #[test]
    fn boolean_logic_and_negation() {
        assert_eq!(
            run("color:blue or shape:orb"),
            ["Blue Gizmo", "Red Wobble", "100% Pure"]
        );
        assert_eq!(run("gizmo -color:red"), ["Blue Gizmo"]);
        assert_eq!(
            run("(color:red or color:blue) weight!=5"),
            ["Blue Gizmo", "Red Wobble"]
        );
        assert_eq!(run("-wobble color:red"), ["Red Gizmo"]);
    }

    #[test]
    fn like_wildcards_are_literal() {
        assert_eq!(run("\"100%\""), ["100% Pure"]);
        assert_eq!(run("\"%\""), ["100% Pure"]);
    }

    #[test]
    fn unknown_term_names_known_terms() {
        let e = compile("flavor:sweet", &vocab()).unwrap_err();
        let msg = e.to_string();
        assert!(matches!(e, SearchError::UnknownTerm { .. }));
        for t in ["color", "c", "weight", "w", "shape", "flavor"] {
            assert!(msg.contains(t), "{msg}");
        }
    }

    #[test]
    fn parse_errors() {
        for q in [
            "",
            "color:",
            "(a",
            "a)",
            "\"open",
            "weight>x",
            "color>red",
            ":x",
            "a or",
        ] {
            let r = compile(q, &vocab());
            assert!(
                matches!(r, Err(SearchError::Parse { .. })),
                "{q:?} -> {r:?}"
            );
        }
    }

    #[test]
    fn injection_is_a_value() {
        assert!(run("\"x'; DROP TABLE g; --\"").is_empty());
    }

    #[test]
    fn vocabulary_is_validated() {
        let bad = |col: &str| {
            Vocabulary::new("name", vec![SearchTerm::new("a", TermKind::Text, col, "")])
        };
        assert!(bad("x; DROP").is_err());
        assert!(
            Vocabulary::new("name", vec![SearchTerm::new("or", TermKind::Text, "x", "")]).is_err()
        );
        assert!(
            Vocabulary::new(
                "name",
                vec![
                    SearchTerm::new("a", TermKind::Text, "x", ""),
                    SearchTerm::new("b", TermKind::Text, "y", "").alias("A"),
                ]
            )
            .is_err()
        );
    }
}

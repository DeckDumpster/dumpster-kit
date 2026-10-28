use dumpster_kit_catalog::{SearchError, SearchTerm, TermKind, Vocabulary, compile};
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
    let bad =
        |col: &str| Vocabulary::new("name", vec![SearchTerm::new("a", TermKind::Text, col, "")]);
    assert!(bad("x; DROP").is_err());
    assert!(Vocabulary::new("name", vec![SearchTerm::new("or", TermKind::Text, "x", "")]).is_err());
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

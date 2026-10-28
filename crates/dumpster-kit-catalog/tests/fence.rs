//! The kit never names a game field: no known game attribute appears in this crate's source.

#[test]
fn no_game_attribute_in_kit_source() {
    const BANNED: &[&str] = &[
        "aspect",
        "supertype",
        "oracle_id",
        "pokedex",
        "hit_points",
        "arena",
        "attacks",
        "keywords",
    ];
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        let text = std::fs::read_to_string(&p).unwrap().to_lowercase();
        for w in BANNED {
            assert!(
                !text.contains(w),
                "{} names game attribute {w:?}",
                p.display()
            );
        }
    }
}

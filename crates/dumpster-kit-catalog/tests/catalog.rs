use std::cell::RefCell;
use std::collections::HashMap;
use std::fs;

use dumpster_kit_catalog::fake::{FakeOnCopy, FakeOnPrinting};
use dumpster_kit_catalog::{
    ALARM_APPLY_FAILED, ALARM_FETCH_FAILED, Catalog, FinishPlacement, Game, RunContext,
    SourceOutcome,
};
use dumpster_kit_tcgcsv::{Error as FetchError, Fetch};

#[derive(Default)]
struct Feed(RefCell<HashMap<String, Option<Vec<u8>>>>);

impl Feed {
    fn set(&self, loc: &str, body: &str) {
        self.0
            .borrow_mut()
            .insert(loc.into(), Some(body.as_bytes().to_vec()));
    }
    fn fail(&self, loc: &str) {
        self.0.borrow_mut().insert(loc.into(), None);
    }
}

impl Fetch for Feed {
    fn get(&self, url: &str) -> dumpster_kit_tcgcsv::Result<Vec<u8>> {
        match self.0.borrow().get(url) {
            Some(Some(b)) => Ok(b.clone()),
            _ => Err(FetchError::Fetch {
                url: url.into(),
                message: "down".into(),
            }),
        }
    }
}

const BODY: &str =
    "thing\tt1\tAlpha\t3\nthing\tt2\tBeta\t5\nvariant\tt1\tplain\nvariant\tt1\tshiny\n";

fn ctx<'a>(day: &'a str) -> RunContext<'a> {
    RunContext { day, at: day }
}

fn count(c: &rusqlite::Connection, table: &str) -> i64 {
    c.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
        .unwrap()
}

#[test]
fn fakes_differ_in_finish_placement() {
    assert_ne!(FakeOnPrinting::FINISH, FakeOnCopy::FINISH);
    assert_eq!(FakeOnCopy::FINISH, FinishPlacement::OnCopy);
}

#[test]
fn applying_the_fake_twice_changes_zero_rows() {
    let dir = tempfile::tempdir().unwrap();
    let cat =
        Catalog::<FakeOnPrinting>::open(&dir.path().join("shared.sqlite"), &dir.path().join("raw"))
            .unwrap();
    let feed = Feed::default();
    feed.set("fake://feed", BODY);

    let first = cat.run(&feed, ctx("2026-10-01")).unwrap();
    let SourceOutcome::Applied { report, .. } = &first[0] else {
        panic!("{first:?}")
    };
    assert_eq!(report.tables["fp_things"].inserted, 2);
    assert_eq!(report.tables["fp_variants"].inserted, 2);

    let second = cat.run(&feed, ctx("2026-10-02")).unwrap();
    assert!(
        matches!(second[0], SourceOutcome::Unchanged { .. }),
        "{second:?}"
    );
    assert_eq!(count(cat.conn(), "applied_payloads"), 1);
}

#[test]
fn reapplying_past_the_ledger_changes_zero_rows() {
    let dir = tempfile::tempdir().unwrap();
    let cat =
        Catalog::<FakeOnPrinting>::open(&dir.path().join("shared.sqlite"), &dir.path().join("raw"))
            .unwrap();
    let feed = Feed::default();
    feed.set("fake://feed", BODY);
    cat.run(&feed, ctx("2026-10-01")).unwrap();
    cat.conn()
        .execute("DELETE FROM applied_payloads", [])
        .unwrap();

    let again = cat.run(&feed, ctx("2026-10-02")).unwrap();
    let SourceOutcome::Applied { report, .. } = &again[0] else {
        panic!("{again:?}")
    };
    assert_eq!(report.total(), 0, "{report:?}");
    assert_eq!(count(cat.conn(), "fp_things"), 2);
}

#[test]
fn a_changed_payload_reports_only_the_rows_that_changed() {
    let dir = tempfile::tempdir().unwrap();
    let cat =
        Catalog::<FakeOnPrinting>::open(&dir.path().join("shared.sqlite"), &dir.path().join("raw"))
            .unwrap();
    let feed = Feed::default();
    feed.set("fake://feed", BODY);
    cat.run(&feed, ctx("2026-10-01")).unwrap();
    feed.set("fake://feed", &BODY.replace("Beta\t5", "Beta\t6"));
    let out = cat.run(&feed, ctx("2026-10-02")).unwrap();
    let SourceOutcome::Applied { report, .. } = &out[0] else {
        panic!("{out:?}")
    };
    assert_eq!(report.tables["fp_things"].updated, 1);
    assert_eq!(report.tables["fp_things"].inserted, 0);
    assert_eq!(report.total(), 1);
}

#[test]
fn a_mismatched_schema_is_rebuilt_from_raw_and_the_tenant_file_is_untouched() {
    struct V2;
    impl Game for V2 {
        const ID: &'static str = FakeOnPrinting::ID;
        const SCHEMA_VERSION: u32 = 2;
        const SCHEMA: &'static str = FakeOnPrinting::SCHEMA;
        const FINISH: FinishPlacement = FakeOnPrinting::FINISH;
        type Payload = <FakeOnPrinting as Game>::Payload;
        fn sources() -> Vec<dumpster_kit_catalog::Source> {
            FakeOnPrinting::sources()
        }
        fn parse(s: &str, r: &[u8]) -> dumpster_kit_catalog::Result<Self::Payload> {
            FakeOnPrinting::parse(s, r)
        }
        fn apply(
            tx: &rusqlite::Transaction<'_>,
            p: &Self::Payload,
        ) -> dumpster_kit_catalog::Result<dumpster_kit_catalog::ApplyReport> {
            FakeOnPrinting::apply(tx, p)
        }
        fn crosswalk(
            p: &dumpster_kit_tcgcsv::Product,
            q: Option<&dumpster_kit_tcgcsv::Price>,
        ) -> dumpster_kit_catalog::Match {
            FakeOnPrinting::crosswalk(p, q)
        }
        fn search_terms() -> Vec<String> {
            FakeOnPrinting::search_terms()
        }
        fn tcgcsv_category() -> i64 {
            FakeOnPrinting::tcgcsv_category()
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("shared.sqlite");
    let raw = dir.path().join("raw");
    let tenant = dir.path().join("tenant-01.sqlite");
    fs::write(&tenant, b"tenant bytes the kit must never touch").unwrap();
    let before = fs::read(&tenant).unwrap();

    {
        let cat = Catalog::<FakeOnPrinting>::open(&db, &raw).unwrap();
        assert!(cat.was_rebuilt(), "a new file is built");
        let feed = Feed::default();
        feed.set("fake://feed", BODY);
        cat.run(&feed, ctx("2026-10-01")).unwrap();
    }
    assert!(
        !Catalog::<FakeOnPrinting>::open(&db, &raw)
            .unwrap()
            .was_rebuilt()
    );

    let cat = Catalog::<V2>::open(&db, &raw).unwrap();
    assert!(cat.was_rebuilt());
    assert_eq!(count(cat.conn(), "fp_things"), 2);
    assert_eq!(count(cat.conn(), "fp_variants"), 2);
    assert_eq!(count(cat.conn(), "applied_payloads"), 1);
    let v: u32 = cat
        .conn()
        .query_row("SELECT schema_version FROM catalog_schema", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(v, 2);
    assert_eq!(fs::read(&tenant).unwrap(), before);
}

#[test]
fn an_edited_schema_with_the_same_version_is_still_rebuilt() {
    struct Edited;
    impl Game for Edited {
        const ID: &'static str = FakeOnPrinting::ID;
        const SCHEMA_VERSION: u32 = 1;
        const SCHEMA: &'static str = "CREATE TABLE fp_things (thing_id TEXT PRIMARY KEY, name TEXT NOT NULL, size INTEGER NOT NULL, extra TEXT) STRICT;
            CREATE TABLE fp_variants (thing_id TEXT NOT NULL, label TEXT NOT NULL, PRIMARY KEY (thing_id, label)) STRICT;";
        const FINISH: FinishPlacement = FinishPlacement::OnPrinting;
        type Payload = <FakeOnPrinting as Game>::Payload;
        fn sources() -> Vec<dumpster_kit_catalog::Source> {
            FakeOnPrinting::sources()
        }
        fn parse(s: &str, r: &[u8]) -> dumpster_kit_catalog::Result<Self::Payload> {
            FakeOnPrinting::parse(s, r)
        }
        fn apply(
            tx: &rusqlite::Transaction<'_>,
            p: &Self::Payload,
        ) -> dumpster_kit_catalog::Result<dumpster_kit_catalog::ApplyReport> {
            FakeOnPrinting::apply(tx, p)
        }
        fn crosswalk(
            p: &dumpster_kit_tcgcsv::Product,
            q: Option<&dumpster_kit_tcgcsv::Price>,
        ) -> dumpster_kit_catalog::Match {
            FakeOnPrinting::crosswalk(p, q)
        }
        fn search_terms() -> Vec<String> {
            vec![]
        }
        fn tcgcsv_category() -> i64 {
            0
        }
    }
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("shared.sqlite");
    let raw = dir.path().join("raw");
    Catalog::<FakeOnPrinting>::open(&db, &raw).unwrap();
    assert!(Catalog::<Edited>::open(&db, &raw).unwrap().was_rebuilt());
}

#[test]
fn a_failing_fetch_raises_an_alarm_that_clears_on_recovery() {
    let dir = tempfile::tempdir().unwrap();
    let cat =
        Catalog::<FakeOnPrinting>::open(&dir.path().join("shared.sqlite"), &dir.path().join("raw"))
            .unwrap();
    let feed = Feed::default();
    feed.fail("fake://feed");
    let out = cat.run(&feed, ctx("2026-10-01")).unwrap();
    assert!(matches!(out[0], SourceOutcome::FetchFailed { .. }));
    let alarms = cat.open_alarms().unwrap();
    assert_eq!(alarms.len(), 1);
    assert_eq!(
        (alarms[0].kind.as_str(), alarms[0].subject.as_str()),
        (ALARM_FETCH_FAILED, "feed")
    );

    cat.run(&feed, ctx("2026-10-02")).unwrap();
    assert_eq!(
        cat.open_alarms().unwrap().len(),
        1,
        "a repeat does not stack"
    );

    feed.set("fake://feed", BODY);
    cat.run(&feed, ctx("2026-10-03")).unwrap();
    assert!(cat.open_alarms().unwrap().is_empty());
    let (ok, err): (Option<String>, Option<String>) = cat
        .conn()
        .query_row(
            "SELECT last_success_at, last_error FROM sync_state WHERE source = 'feed'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!(ok.as_deref(), Some("2026-10-03"));
    assert_eq!(err, None);
}

#[test]
fn an_unparseable_payload_rolls_back_alarms_and_does_not_enter_the_ledger() {
    let dir = tempfile::tempdir().unwrap();
    let cat =
        Catalog::<FakeOnPrinting>::open(&dir.path().join("shared.sqlite"), &dir.path().join("raw"))
            .unwrap();
    let feed = Feed::default();
    feed.set("fake://feed", "thing\tt1\tAlpha\tnot-a-number\n");
    let out = cat.run(&feed, ctx("2026-10-01")).unwrap();
    assert!(
        matches!(out[0], SourceOutcome::ApplyFailed { .. }),
        "{out:?}"
    );
    assert_eq!(count(cat.conn(), "fp_things"), 0);
    assert_eq!(count(cat.conn(), "applied_payloads"), 0);
    assert_eq!(cat.open_alarms().unwrap()[0].kind, ALARM_APPLY_FAILED);
}

#[test]
fn a_payload_failing_mid_apply_leaves_no_partial_rows() {
    let dir = tempfile::tempdir().unwrap();
    let cat =
        Catalog::<FakeOnPrinting>::open(&dir.path().join("shared.sqlite"), &dir.path().join("raw"))
            .unwrap();
    let feed = Feed::default();
    // The variant names a thing that does not exist: the foreign key fails after the thing is written.
    feed.set(
        "fake://feed",
        "thing\tt1\tAlpha\t3\nvariant\tghost\tplain\n",
    );
    let out = cat.run(&feed, ctx("2026-10-01")).unwrap();
    assert!(
        matches!(out[0], SourceOutcome::ApplyFailed { .. }),
        "{out:?}"
    );
    assert_eq!(count(cat.conn(), "fp_things"), 0);
    assert_eq!(count(cat.conn(), "applied_payloads"), 0);
}

#[test]
fn raw_is_landed_by_game_source_and_day() {
    let dir = tempfile::tempdir().unwrap();
    let raw = dir.path().join("raw");
    let cat = Catalog::<FakeOnCopy>::open(&dir.path().join("shared.sqlite"), &raw).unwrap();
    let feed = Feed::default();
    feed.set("fake://feed", BODY);
    feed.set("fake://extras", "thing\tx1\tExtra\t1\n");
    cat.run(&feed, ctx("2026-10-01")).unwrap();
    cat.run(&feed, ctx("2026-10-01")).unwrap();
    for source in ["feed", "extras"] {
        let d = raw.join("fakecopy").join(source).join("2026-10-01");
        assert_eq!(
            fs::read_dir(d).unwrap().count(),
            1,
            "{source}: same bytes land once per day"
        );
    }
}

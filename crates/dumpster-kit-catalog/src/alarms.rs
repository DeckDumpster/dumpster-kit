use rusqlite::{Connection, params};

use crate::error::Result;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Alarm {
    pub game: String,
    pub kind: String,
    pub subject: String,
    pub detail: String,
    pub raised_at: String,
}

/// Raise an alarm; one already open for (game, kind, subject) keeps its `raised_at` and only
/// takes the new detail.
pub fn raise(
    conn: &Connection,
    game: &str,
    kind: &str,
    subject: &str,
    detail: &str,
    at: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO alarms (game, kind, subject, detail, raised_at) VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT (game, kind, subject) WHERE cleared_at IS NULL DO UPDATE SET detail = ?4",
        params![game, kind, subject, detail, at],
    )?;
    Ok(())
}

pub fn clear(conn: &Connection, game: &str, kind: &str, subject: &str, at: &str) -> Result<()> {
    conn.execute(
        "UPDATE alarms SET cleared_at = ?4
         WHERE game = ?1 AND kind = ?2 AND subject = ?3 AND cleared_at IS NULL",
        params![game, kind, subject, at],
    )?;
    Ok(())
}

pub fn open_alarms(conn: &Connection, game: &str) -> Result<Vec<Alarm>> {
    let mut stmt = conn.prepare(
        "SELECT game, kind, subject, detail, raised_at FROM alarms
         WHERE game = ?1 AND cleared_at IS NULL ORDER BY id",
    )?;
    let rows = stmt.query_map([game], |r| {
        Ok(Alarm {
            game: r.get(0)?,
            kind: r.get(1)?,
            subject: r.get(2)?,
            detail: r.get(3)?,
            raised_at: r.get(4)?,
        })
    })?;
    Ok(rows.collect::<std::result::Result<_, _>>()?)
}

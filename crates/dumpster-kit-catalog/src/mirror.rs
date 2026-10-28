use dumpster_kit_tcgcsv::Category;
use rusqlite::Transaction;

use crate::error::Result;

/// Mirror tcgcsv's category listing. Upserts; returns the rows inserted or changed.
pub fn mirror_categories(tx: &Transaction<'_>, categories: &[Category]) -> Result<u64> {
    let mut stmt = tx.prepare_cached(
        "INSERT INTO tcgcsv_categories
             (category_id, name, display_name, modified_on, sealed_label, popularity)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT (category_id) DO UPDATE SET
             name = excluded.name, display_name = excluded.display_name,
             modified_on = excluded.modified_on, sealed_label = excluded.sealed_label,
             popularity = excluded.popularity
         WHERE (name, display_name, modified_on, sealed_label, popularity) IS NOT
               (excluded.name, excluded.display_name, excluded.modified_on,
                excluded.sealed_label, excluded.popularity)",
    )?;
    let mut changed = 0;
    for c in categories {
        changed += stmt.execute((
            c.category_id,
            &c.name,
            &c.display_name,
            &c.modified_on,
            &c.sealed_label,
            c.popularity,
        ))? as u64;
    }
    Ok(changed)
}

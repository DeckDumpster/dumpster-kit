use std::fs;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::error::{Error, Result, io};

pub fn hash_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// One payload found under `raw/<game>/<source>/<day>/`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Landed {
    pub source: String,
    pub day: String,
    pub seq: u32,
    pub hash: String,
    pub path: PathBuf,
}

/// Raw bytes exactly as received, kept so the catalog can always be rebuilt from them.
pub struct RawLanding {
    game_dir: PathBuf,
}

impl RawLanding {
    pub fn new(root: &Path, game: &str) -> Self {
        Self {
            game_dir: root.join(game),
        }
    }

    /// Write `bytes` under `<source>/<day>/<seq>-<hash>.bin`. The same bytes landed again on the
    /// same day keep their first file. Written to a temp name and renamed, so a reader never sees
    /// a partial file.
    pub fn land(&self, source: &str, day: &str, bytes: &[u8]) -> Result<Landed> {
        let hash = hash_hex(bytes);
        let dir = self.game_dir.join(source).join(day);
        fs::create_dir_all(&dir).map_err(io(&dir))?;
        let existing = list_dir(&dir, source, day)?;
        if let Some(found) = existing.iter().find(|l| l.hash == hash) {
            return Ok(found.clone());
        }
        let seq = existing.iter().map(|l| l.seq).max().map_or(1, |s| s + 1);
        let path = dir.join(format!("{seq:06}-{hash}.bin"));
        let tmp = dir.join(format!(".{seq:06}-{hash}.tmp"));
        fs::write(&tmp, bytes).map_err(io(&tmp))?;
        fs::rename(&tmp, &path).map_err(io(&path))?;
        Ok(Landed {
            source: source.to_string(),
            day: day.to_string(),
            seq,
            hash,
            path,
        })
    }

    /// Everything landed for this game, oldest first (day, source, seq).
    pub fn all(&self) -> Result<Vec<Landed>> {
        let mut out = Vec::new();
        if !self.game_dir.exists() {
            return Ok(out);
        }
        for source in sorted_dirs(&self.game_dir)? {
            let sname = file_name(&source);
            for day in sorted_dirs(&source)? {
                out.extend(list_dir(&day, &sname, &file_name(&day))?);
            }
        }
        out.sort_by(|a, b| (&a.day, &a.source, a.seq).cmp(&(&b.day, &b.source, b.seq)));
        Ok(out)
    }
}

fn file_name(p: &Path) -> String {
    p.file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

fn sorted_dirs(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut v: Vec<PathBuf> = fs::read_dir(dir)
        .map_err(io(dir))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.is_dir())
        .collect();
    v.sort();
    Ok(v)
}

fn list_dir(dir: &Path, source: &str, day: &str) -> Result<Vec<Landed>> {
    let mut out = Vec::new();
    for entry in fs::read_dir(dir).map_err(io(dir))? {
        let path = entry.map_err(io(dir))?.path();
        let name = file_name(&path);
        if name.starts_with('.') {
            continue;
        }
        let bad = || Error::BadLanding {
            path: path.display().to_string(),
        };
        let stem = name.strip_suffix(".bin").ok_or_else(bad)?;
        let (seq, hash) = stem.split_once('-').ok_or_else(bad)?;
        out.push(Landed {
            source: source.to_string(),
            day: day.to_string(),
            seq: seq.parse().map_err(|_| bad())?,
            hash: hash.to_string(),
            path,
        });
    }
    Ok(out)
}

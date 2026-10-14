//! Contrast is a test, not a review note: WCAG 2.1 AA over every pairing the manifest declares,
//! plus the structural rules that keep the two-layer token split honest.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use crate::tokens::{alias_target, parse, resolve};

#[derive(Debug, Clone, Copy)]
struct Rgba {
    r: f64,
    g: f64,
    b: f64,
    a: f64,
}

fn parse_color(value: &str) -> Result<Rgba, String> {
    let v = value.trim();
    if let Some(h) = v.strip_prefix('#') {
        if !h.bytes().all(|b| b.is_ascii_hexdigit()) || ![3, 4, 6, 8].contains(&h.len()) {
            return Err(format!("cannot parse colour: {value}"));
        }
        let h: String = if h.len() <= 4 {
            h.chars().flat_map(|c| [c, c]).collect()
        } else {
            h.into()
        };
        let n = |i: usize| f64::from(u8::from_str_radix(&h[i..i + 2], 16).unwrap());
        return Ok(Rgba {
            r: n(0),
            g: n(2),
            b: n(4),
            a: if h.len() == 8 { n(6) / 255.0 } else { 1.0 },
        });
    }
    let inner = v
        .strip_prefix("rgba(")
        .or_else(|| v.strip_prefix("rgb("))
        .and_then(|s| s.strip_suffix(')'))
        .ok_or_else(|| format!("cannot parse colour: {value}"))?;
    let parts: Vec<f64> = inner
        .split(|c: char| c == ',' || c == '/' || c.is_whitespace())
        .filter(|s| !s.is_empty())
        .map(|s| {
            s.parse::<f64>()
                .map_err(|_| format!("cannot parse colour: {value}"))
        })
        .collect::<Result<_, _>>()?;
    if parts.len() < 3 {
        return Err(format!("cannot parse colour: {value}"));
    }
    Ok(Rgba {
        r: parts[0],
        g: parts[1],
        b: parts[2],
        a: parts.get(3).copied().unwrap_or(1.0),
    })
}

fn flatten(fg: Rgba, bg: Rgba) -> Rgba {
    if fg.a >= 1.0 {
        return fg;
    }
    Rgba {
        r: fg.r * fg.a + bg.r * (1.0 - fg.a),
        g: fg.g * fg.a + bg.g * (1.0 - fg.a),
        b: fg.b * fg.a + bg.b * (1.0 - fg.a),
        a: 1.0,
    }
}

fn luminance(c: Rgba) -> f64 {
    let lin = |v: f64| {
        let c = v / 255.0;
        if c <= 0.03928 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * lin(c.r) + 0.7152 * lin(c.g) + 0.0722 * lin(c.b)
}

fn ratio(a: Rgba, b: Rgba) -> f64 {
    let (x, y) = (luminance(a), luminance(b));
    (x.max(y) + 0.05) / (x.min(y) + 0.05)
}

/// The needed ratio for a size class: `normal` 4.5 (1.4.3), `large` and `ui` 3.0 (1.4.3, 1.4.11).
fn threshold(size: &str) -> Option<f64> {
    match size {
        "normal" => Some(4.5),
        "large" | "ui" => Some(3.0),
        _ => None,
    }
}

/// One evaluated pairing, for a report.
#[derive(Debug, Clone)]
pub struct Measured {
    pub label: String,
    pub ratio: f64,
    pub needs: f64,
}

struct Pair<'a> {
    fg: &'a str,
    bg: &'a str,
    base: Option<&'a str>,
    size: &'a str,
}

impl Pair<'_> {
    fn label(&self) -> String {
        match self.base {
            Some(b) => format!("{} on {} over {b}", self.fg, self.bg),
            None => format!("{} on {}", self.fg, self.bg),
        }
    }
}

fn pairs(manifest: &Value) -> Result<Vec<Pair<'_>>, String> {
    let list = manifest
        .get("pairs")
        .and_then(Value::as_array)
        .ok_or("manifest has no \"pairs\" array")?;
    list.iter()
        .map(|p| {
            let s = |k: &str| p.get(k).and_then(Value::as_str);
            Ok(Pair {
                fg: s("fg").ok_or("pairing without \"fg\"")?,
                bg: s("bg").ok_or("pairing without \"bg\"")?,
                base: s("base"),
                size: s("size").ok_or("pairing without \"size\"")?,
            })
        })
        .collect()
}

/// Measure every pairing in the manifest. An unparseable or undeclared token is an `Err`.
pub fn measure(
    tokens: &BTreeMap<String, String>,
    manifest: &Value,
) -> Result<Vec<Measured>, String> {
    let mut out = Vec::new();
    for p in pairs(manifest)? {
        let needs = threshold(p.size)
            .ok_or_else(|| format!("unknown size class \"{}\" on {}", p.size, p.label()))?;
        let mut bg = parse_color(&resolve(tokens, p.bg)?)?;
        if bg.a < 1.0 {
            let base = p.base.ok_or_else(|| {
                format!(
                    "{} is translucent — the pairing needs a \"base\" surface",
                    p.bg
                )
            })?;
            let base = parse_color(&resolve(tokens, base)?)?;
            if base.a < 1.0 {
                return Err(format!("the base of {} must be opaque", p.label()));
            }
            bg = flatten(bg, base);
        }
        let fg = flatten(parse_color(&resolve(tokens, p.fg)?)?, bg);
        out.push(Measured {
            label: p.label(),
            ratio: ratio(fg, bg),
            needs,
        });
    }
    Ok(out)
}

/// Audit a token file against a pairing manifest. Returns every problem, empty when clean.
pub fn audit(tokens_css: &str, manifest_json: &str) -> Vec<String> {
    let tokens = parse(tokens_css);
    let manifest: Value = match serde_json::from_str(manifest_json) {
        Ok(v) => v,
        Err(e) => return vec![format!("manifest is not JSON: {e}")],
    };
    let mut problems = Vec::new();

    match measure(&tokens, &manifest) {
        Ok(all) => problems.extend(
            all.into_iter()
                .filter(|m| m.ratio + 1e-9 < m.needs)
                .map(|m| format!("{}: {:.2}:1 — needs {:.1}:1", m.label, m.ratio, m.needs)),
        ),
        Err(e) => problems.push(e),
    }

    // The split: semantic colour tokens are pure aliases of the reference layer.
    for (name, value) in &tokens {
        if name.starts_with("--color-")
            && !alias_target(value).is_some_and(|t| t.starts_with("--pd-"))
        {
            problems.push(format!(
                "{name}: {value} — semantic --color-* tokens must be var(--pd-*) aliases"
            ));
        }
    }

    // No reference colour token without a semantic role pointing at it.
    let referenced: BTreeSet<&str> = tokens
        .iter()
        .filter(|(n, _)| !n.starts_with("--pd-"))
        .flat_map(|(_, v)| {
            v.split("var(")
                .skip(1)
                .filter_map(|r| r.split([')', ',']).next())
        })
        .map(str::trim)
        .collect();
    for name in tokens
        .keys()
        .filter(|n| n.starts_with("--pd-") && !referenced.contains(n.as_str()))
    {
        problems.push(format!(
            "{name}: reference token no semantic role points at"
        ));
    }

    // Each ramp `--pd-<family>-<step>` is monotonic: lower step = lighter.
    let mut families: BTreeMap<&str, Vec<(u32, &str)>> = BTreeMap::new();
    for (name, value) in &tokens {
        let Some((family, step)) = name.strip_prefix("--pd-").and_then(|r| r.rsplit_once('-'))
        else {
            continue;
        };
        let (Ok(step), true) = (
            step.parse::<u32>(),
            family.bytes().all(|b| b.is_ascii_lowercase()),
        ) else {
            continue;
        };
        if parse_color(value).is_ok_and(|c| c.a >= 1.0) {
            families.entry(family).or_default().push((step, name));
        }
    }
    for (family, mut steps) in families {
        steps.sort();
        for w in steps.windows(2) {
            let lum = |n: &str| {
                resolve(&tokens, n)
                    .and_then(|v| parse_color(&v))
                    .map(luminance)
            };
            if let (Ok(lighter), Ok(darker)) = (lum(w[0].1), lum(w[1].1))
                && lighter <= darker
            {
                problems.push(format!(
                    "--pd-{family}: {} must be lighter than {}",
                    w[0].1, w[1].1
                ));
            }
        }
    }

    // Every colour role is paired or explicitly exempt; the lists agree with the file.
    let mut used = BTreeSet::new();
    if let Ok(list) = pairs(&manifest) {
        for p in &list {
            used.extend([p.fg, p.bg]);
            used.extend(p.base);
        }
        let mut seen = BTreeSet::new();
        for p in &list {
            for t in [Some(p.fg), Some(p.bg), p.base].into_iter().flatten() {
                if !tokens.contains_key(t) {
                    problems.push(format!("pairing names unknown token {t}"));
                }
            }
            if !seen.insert(p.label()) {
                problems.push(format!("duplicate pairing: {}", p.label()));
            }
        }
    }
    let exempt: BTreeSet<&str> = manifest
        .get("exempt")
        .and_then(Value::as_object)
        .map(|o| {
            o.keys()
                .map(String::as_str)
                .filter(|k| k.starts_with("--"))
                .collect()
        })
        .unwrap_or_default();
    for name in tokens.keys().filter(|n| n.starts_with("--color-")) {
        if !used.contains(name.as_str()) && !exempt.contains(name.as_str()) {
            problems.push(format!(
                "{name}: colour role neither contrast-tested nor listed in \"exempt\""
            ));
        }
    }
    for name in &exempt {
        if !tokens.contains_key(*name) {
            problems.push(format!("\"exempt\" lists {name}, which no longer exists"));
        }
        if used.contains(name) {
            problems.push(format!("{name} is both exempt and paired"));
        }
    }
    problems
}

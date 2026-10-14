//! The enforcement: outside the token file, no colour, space, type, radius or elevation value is
//! spelled out, and nothing reaches into the reference layer. There is no budget and no ratchet —
//! any hit is a failure.
//!
//! Scope: `.svelte .ts .js .css .svg .html`, comments excluded. A declaration counts when its value
//! spells out a length (px/rem/em/%/…) outside a plain `var(--x)`. A `var(--x, 8px)` fallback is a
//! second value chosen raw, so it counts. Unitless `0` and bare `calc()` multipliers do not.

use std::fmt;
use std::path::{Path, PathBuf};

use crate::tokens::blank_comments;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Colour,
    Space,
    Type,
    Radius,
    Elevation,
    ReferenceLayer,
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Kind::Colour => "raw colour",
            Kind::Space => "raw space",
            Kind::Type => "raw type size",
            Kind::Radius => "raw radius",
            Kind::Elevation => "raw elevation",
            Kind::ReferenceLayer => "reference-layer token",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    pub file: PathBuf,
    pub line: usize,
    pub kind: Kind,
    pub text: String,
}

impl fmt::Display for Violation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}:{}: {}: {}",
            self.file.display(),
            self.line,
            self.kind,
            self.text
        )
    }
}

const SCANNED: &[&str] = &["svelte", "ts", "js", "css", "svg", "html"];
const SKIPPED_DIRS: &[&str] = &[
    "node_modules",
    ".svelte-kit",
    "build",
    "dist",
    "target",
    ".git",
];

const NAMED: &[&str] = &[
    "aliceblue",
    "antiquewhite",
    "aqua",
    "aquamarine",
    "azure",
    "beige",
    "bisque",
    "black",
    "blanchedalmond",
    "blue",
    "blueviolet",
    "brown",
    "burlywood",
    "cadetblue",
    "chartreuse",
    "chocolate",
    "coral",
    "cornflowerblue",
    "cornsilk",
    "crimson",
    "cyan",
    "darkblue",
    "darkcyan",
    "darkgoldenrod",
    "darkgray",
    "darkgrey",
    "darkgreen",
    "darkkhaki",
    "darkmagenta",
    "darkolivegreen",
    "darkorange",
    "darkorchid",
    "darkred",
    "darksalmon",
    "darkseagreen",
    "darkslateblue",
    "darkslategray",
    "darkslategrey",
    "darkturquoise",
    "darkviolet",
    "deeppink",
    "deepskyblue",
    "dimgray",
    "dimgrey",
    "dodgerblue",
    "firebrick",
    "floralwhite",
    "forestgreen",
    "fuchsia",
    "gainsboro",
    "ghostwhite",
    "gold",
    "goldenrod",
    "gray",
    "grey",
    "green",
    "greenyellow",
    "honeydew",
    "hotpink",
    "indianred",
    "indigo",
    "ivory",
    "khaki",
    "lavender",
    "lavenderblush",
    "lawngreen",
    "lemonchiffon",
    "lightblue",
    "lightcoral",
    "lightcyan",
    "lightgoldenrodyellow",
    "lightgray",
    "lightgrey",
    "lightgreen",
    "lightpink",
    "lightsalmon",
    "lightseagreen",
    "lightskyblue",
    "lightslategray",
    "lightslategrey",
    "lightsteelblue",
    "lightyellow",
    "lime",
    "limegreen",
    "linen",
    "magenta",
    "maroon",
    "mediumaquamarine",
    "mediumblue",
    "mediumorchid",
    "mediumpurple",
    "mediumseagreen",
    "mediumslateblue",
    "mediumspringgreen",
    "mediumturquoise",
    "mediumvioletred",
    "midnightblue",
    "mintcream",
    "mistyrose",
    "moccasin",
    "navajowhite",
    "navy",
    "oldlace",
    "olive",
    "olivedrab",
    "orange",
    "orangered",
    "orchid",
    "palegoldenrod",
    "palegreen",
    "paleturquoise",
    "palevioletred",
    "papayawhip",
    "peachpuff",
    "peru",
    "pink",
    "plum",
    "powderblue",
    "purple",
    "rebeccapurple",
    "red",
    "rosybrown",
    "royalblue",
    "saddlebrown",
    "salmon",
    "sandybrown",
    "seagreen",
    "seashell",
    "sienna",
    "silver",
    "skyblue",
    "slateblue",
    "slategray",
    "slategrey",
    "snow",
    "springgreen",
    "steelblue",
    "tan",
    "teal",
    "thistle",
    "tomato",
    "turquoise",
    "violet",
    "wheat",
    "white",
    "whitesmoke",
    "yellow",
    "yellowgreen",
];

const COLOUR_FUNCTIONS: &[&str] = &[
    "rgb", "rgba", "hsl", "hsla", "hwb", "lab", "lch", "oklab", "oklch", "color",
];
const UNITS: &[&str] = &[
    "px", "rem", "em", "%", "vh", "vw", "vmin", "vmax", "ch", "ex", "pt", "cm", "mm", "in", "pc",
    "q", "svh", "dvh", "lvh", "svw", "dvw", "lvw", "cqw", "cqh",
];

fn is_word(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// Comments blanked, including whole lines that start with `//` or `*`.
fn decomment(text: &str) -> String {
    blank_comments(text)
        .split('\n')
        .map(|l| {
            let t = l.trim_start();
            if t.starts_with("//") || t.starts_with('*') {
                " ".repeat(l.len())
            } else {
                l.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn line_of(text: &str, offset: usize) -> usize {
    text[..offset].bytes().filter(|&b| b == b'\n').count() + 1
}

/// Replace every balanced `name(...)` whose name is accepted by `pick` with spaces of the same width.
fn blank_calls(text: &str, pick: impl Fn(&str, &str) -> bool) -> String {
    let b = text.as_bytes();
    let mut out = text.as_bytes().to_vec();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'(' {
            let mut s = i;
            while s > 0 && (b[s - 1].is_ascii_alphabetic() || b[s - 1] == b'-') {
                s -= 1;
            }
            let mut depth = 0;
            let mut close = None;
            for (e, &c) in b.iter().enumerate().skip(i) {
                match c {
                    b'(' => depth += 1,
                    b')' => {
                        depth -= 1;
                        if depth == 0 {
                            close = Some(e);
                            break;
                        }
                    }
                    _ => {}
                }
            }
            let Some(e) = close else {
                i += 1;
                continue;
            };
            if s < i && pick(&text[s..i], &text[i + 1..e]) {
                for o in out.iter_mut().take(e + 1).skip(s) {
                    if *o != b'\n' {
                        *o = b' ';
                    }
                }
                i = e;
            }
        }
        i += 1;
    }
    String::from_utf8(out).expect("blanking ascii bytes keeps utf-8")
}

/// A plain `var(--x)` carries no fallback and so no value of its own.
fn devar(value: &str) -> String {
    blank_calls(value, |name, inner| {
        name == "var" && inner.trim().starts_with("--") && !inner.contains(',')
    })
}

fn has_length(value: &str) -> bool {
    let b = value.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let starts = b[i].is_ascii_digit()
            || (b[i] == b'.' && b.get(i + 1).is_some_and(u8::is_ascii_digit))
            || (b[i] == b'-'
                && b.get(i + 1)
                    .is_some_and(|c| c.is_ascii_digit() || *c == b'.'));
        if starts && (i == 0 || !(is_word(b[i - 1]) || matches!(b[i - 1], b'.' | b'#' | b'-'))) {
            let mut j = i + usize::from(b[i] == b'-');
            while j < b.len() && (b[j].is_ascii_digit() || b[j] == b'.') {
                j += 1;
            }
            let u = j;
            if b.get(j) == Some(&b'%') {
                return true;
            }
            while j < b.len() && b[j].is_ascii_alphabetic() {
                j += 1;
            }
            let unit = value[u..j].to_ascii_lowercase();
            if UNITS.contains(&unit.as_str())
                && b.get(j).is_none_or(|c| !(is_word(*c) || *c == b'-'))
            {
                return true;
            }
            i = j.max(i + 1);
        } else {
            i += 1;
        }
    }
    false
}

fn dimension_kind(prop: &str) -> Option<Kind> {
    let p = prop;
    if p == "font-size" {
        return Some(Kind::Type);
    }
    if p == "box-shadow" {
        return Some(Kind::Elevation);
    }
    if p.ends_with("radius") && (p == "border-radius" || p.starts_with("border-")) {
        return Some(Kind::Radius);
    }
    let sides = ["top", "right", "bottom", "left", "inline", "block"];
    for base in ["padding", "margin"] {
        if p == base {
            return Some(Kind::Space);
        }
        if let Some(rest) = p.strip_prefix(base).and_then(|r| r.strip_prefix('-')) {
            let rest = rest
                .strip_suffix("-start")
                .or_else(|| rest.strip_suffix("-end"))
                .unwrap_or(rest);
            if sides.contains(&rest) {
                return Some(Kind::Space);
            }
        }
    }
    matches!(p, "gap" | "row-gap" | "column-gap").then_some(Kind::Space)
}

fn paints_colour(prop: &str) -> bool {
    matches!(
        prop,
        "color"
            | "fill"
            | "stroke"
            | "background"
            | "border"
            | "outline"
            | "box-shadow"
            | "text-shadow"
            | "text-decoration"
            | "column-rule"
    ) || prop.ends_with("-color")
        || prop.starts_with("border-")
        || prop.starts_with("outline-")
}

fn named_colour(value: &str) -> Option<String> {
    let value = blank_calls(value, |name, _| matches!(name, "var" | "url"));
    value
        .split(|c: char| !(c.is_ascii_alphabetic() || c == '-'))
        .find(|w| NAMED.contains(&w.to_ascii_lowercase().as_str()))
        .map(str::to_string)
}

/// Lint one file's text. `file` is only a label for the violations.
pub fn lint_source(file: &Path, text: &str) -> Vec<Violation> {
    let body = decomment(text);
    let b = body.as_bytes();
    let mut out = Vec::new();
    let mut push = |off: usize, kind, t: &str| {
        out.push(Violation {
            file: file.to_path_buf(),
            line: line_of(&body, off),
            kind,
            text: t.trim().to_string(),
        })
    };

    // Hex colours.
    for (i, _) in body.match_indices('#') {
        if i > 0 && (is_word(b[i - 1]) || b[i - 1] == b'&') {
            continue;
        }
        let digits = b[i + 1..]
            .iter()
            .take_while(|c| c.is_ascii_hexdigit())
            .count();
        let after = b.get(i + 1 + digits).copied();
        if [3, 4, 6, 8].contains(&digits) && after.is_none_or(|c| !(is_word(c) || c == b'-')) {
            push(i, Kind::Colour, &body[i..i + 1 + digits]);
        }
    }
    // Colour functions.
    for (i, _) in body.match_indices('(') {
        let mut s = i;
        while s > 0 && b[s - 1].is_ascii_alphabetic() {
            s -= 1;
        }
        if COLOUR_FUNCTIONS.contains(&body[s..i].to_ascii_lowercase().as_str())
            && (s == 0 || !matches!(b[s - 1], b'-' | b'_'))
        {
            let end = body[i..].find(')').map_or(body.len(), |e| i + e + 1);
            push(s, Kind::Colour, &body[s..end]);
        }
    }
    // Reference layer.
    for (i, _) in body.match_indices("var(") {
        let rest = body[i + 4..].trim_start();
        if rest.starts_with("--pd-") {
            push(
                i,
                Kind::ReferenceLayer,
                &body[i..i
                    + 4
                    + (body.len() - i - 4 - rest.len())
                    + rest.find(')').map_or(rest.len(), |e| e + 1)],
            );
        }
    }
    // Declarations: `property: value` / `property="value"`.
    let mut i = 0;
    while i < b.len() {
        if b[i].is_ascii_lowercase() && (i == 0 || !(is_word(b[i - 1]) || b[i - 1] == b'-')) {
            let mut j = i;
            while j < b.len() && (b[j].is_ascii_lowercase() || b[j] == b'-') {
                j += 1;
            }
            let prop = &body[i..j];
            let mut k = j;
            while k < b.len() && b[k] == b' ' {
                k += 1;
            }
            let sep = b.get(k).copied();
            if matches!(sep, Some(b':') | Some(b'='))
                && (dimension_kind(prop).is_some() || paints_colour(prop))
            {
                k += 1;
                while k < b.len() && matches!(b[k], b' ' | b'\t' | b'"' | b'\'') {
                    k += 1;
                }
                let end = body[k..]
                    .find([';', '\n', '{', '}', '"', '\''])
                    .map_or(body.len(), |e| k + e);
                let value = &body[k..end];
                if let Some(kind) = dimension_kind(prop)
                    && sep == Some(b':')
                    && has_length(&devar(value))
                {
                    push(i, kind, &format!("{prop}: {}", value.trim()));
                }
                if paints_colour(prop)
                    && let Some(w) = named_colour(value)
                {
                    push(i, Kind::Colour, &format!("{prop}: {} ({w})", value.trim()));
                }
            }
            i = j;
        } else {
            i += 1;
        }
    }
    out.sort_by(|a, b| (a.line, a.text.as_str()).cmp(&(b.line, b.text.as_str())));
    out.dedup();
    out
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    let mut entries: Vec<_> = std::fs::read_dir(dir)?.collect::<Result<_, _>>()?;
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let p = e.path();
        if p.is_dir() {
            if !SKIPPED_DIRS.iter().any(|s| e.file_name() == *s) {
                walk(&p, out)?;
            }
        } else if p
            .extension()
            .and_then(|x| x.to_str())
            .is_some_and(|x| SCANNED.contains(&x))
        {
            out.push(p);
        }
    }
    Ok(())
}

/// Lint every scanned file under `roots`, except the named token files (`exempt`, compared by
/// canonical path). Violations are in file order. A root that cannot be read is an error, not a
/// clean result.
pub fn lint_tree(roots: &[PathBuf], exempt: &[PathBuf]) -> std::io::Result<Vec<Violation>> {
    let exempt: Vec<PathBuf> = exempt
        .iter()
        .map(std::fs::canonicalize)
        .collect::<Result<_, _>>()?;
    let mut files = Vec::new();
    for r in roots {
        if r.is_file() {
            files.push(r.clone());
        } else {
            walk(r, &mut files)?;
        }
    }
    let mut out = Vec::new();
    for f in files {
        if exempt.contains(&std::fs::canonicalize(&f)?) {
            continue;
        }
        out.extend(lint_source(&f, &std::fs::read_to_string(&f)?));
    }
    Ok(out)
}

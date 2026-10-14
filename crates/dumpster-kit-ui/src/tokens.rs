//! Reading a token file: custom-property declarations, resolved through `var()` aliases.

use std::collections::BTreeMap;

/// Replace every comment with spaces, keeping newlines, so offsets and line numbers still match the
/// original. Handles `/* */` and `<!-- -->`.
pub fn blank_comments(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while !rest.is_empty() {
        let open = [("/*", "*/"), ("<!--", "-->")]
            .into_iter()
            .filter_map(|(o, c)| rest.find(o).map(|i| (i, o, c)))
            .min_by_key(|(i, _, _)| *i);
        let Some((i, o, c)) = open else {
            out.push_str(rest);
            break;
        };
        out.push_str(&rest[..i]);
        let after = &rest[i + o.len()..];
        let end = after.find(c).map_or(after.len(), |j| j + c.len());
        let comment = &rest[i..i + o.len() + end];
        out.extend(
            comment
                .chars()
                .map(|ch| if ch == '\n' { '\n' } else { ' ' }),
        );
        rest = &rest[i + o.len() + end..];
    }
    out
}

/// Every `--name: value;` declaration, last write wins. Names are in sorted order.
pub fn parse(css: &str) -> BTreeMap<String, String> {
    let css = blank_comments(css);
    let bytes = css.as_bytes();
    let mut out = BTreeMap::new();
    let mut i = 0;
    while i + 2 < bytes.len() {
        if bytes[i] == b'-' && bytes[i + 1] == b'-' && (i == 0 || !is_name(bytes[i - 1])) {
            let start = i;
            let mut j = i + 2;
            while j < bytes.len() && is_name(bytes[j]) {
                j += 1;
            }
            let name = &css[start..j];
            let mut k = j;
            while k < bytes.len() && bytes[k].is_ascii_whitespace() {
                k += 1;
            }
            if name.len() > 2
                && k < bytes.len()
                && bytes[k] == b':'
                && let Some(end) = css[k + 1..].find(';')
            {
                out.insert(name.to_string(), css[k + 1..k + 1 + end].trim().to_string());
                i = k + 1 + end;
                continue;
            }
            i = j;
        } else {
            i += 1;
        }
    }
    out
}

fn is_name(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'-' || b == b'_'
}

/// `var(--x)` with no fallback → `--x`.
pub fn alias_target(value: &str) -> Option<&str> {
    let inner = value.trim().strip_prefix("var(")?.strip_suffix(')')?.trim();
    (inner.starts_with("--") && inner.bytes().all(is_name)).then_some(inner)
}

/// Follow `var()` aliases to a literal value.
pub fn resolve(tokens: &BTreeMap<String, String>, name: &str) -> Result<String, String> {
    let mut seen = vec![name.to_string()];
    let mut cur = name.to_string();
    loop {
        let value = tokens
            .get(&cur)
            .ok_or_else(|| format!("token {cur} is not declared in tokens.css"))?;
        match alias_target(value) {
            Some(next) => {
                if seen.iter().any(|s| s == next) {
                    return Err(format!(
                        "cyclic token alias: {} -> {next}",
                        seen.join(" -> ")
                    ));
                }
                seen.push(next.to_string());
                cur = next.to_string();
            }
            None => return Ok(value.clone()),
        }
    }
}

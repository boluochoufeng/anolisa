//! chrome-devtools-mcp uid-snapshot parser: flat indented listing whose
//! quoted names may contain raw newlines. A whole-text cursor consumes each
//! record's exact span; every record is rebuilt byte-for-byte from its
//! decomposed fields before it is accepted, so continuation lines can never
//! be silently dropped.

use super::{Format, Parsed, Record, is_diff_wrapper, scan_double_quoted};

/// Parses a chrome-devtools snapshot (optionally preceded by action-result
/// lines), or `None` for any other shape.
pub(crate) fn parse(input: &str) -> Option<Parsed> {
    let line_starts = line_start_offsets(input);
    let first = line_starts
        .iter()
        .position(|&s| input[s..].trim_start().starts_with("uid="))?;
    let prefix = input[..line_starts[first]].to_owned();
    if is_diff_wrapper(&prefix) {
        return None;
    }

    let mut records: Vec<Record> = Vec::new();
    let mut raws: Vec<String> = Vec::new();
    let mut li = first;
    let mut consumed_end = line_starts[first];
    while li < line_starts.len() {
        let start = line_starts[li];
        let line_end = input[start..]
            .find('\n')
            .map(|i| start + i)
            .unwrap_or(input.len());
        let line = &input[start..line_end];
        if line.trim().is_empty() {
            if let Some(raw) = raws.last_mut() {
                raw.push('\n');
                raw.push_str(line);
            }
            li += 1;
            consumed_end = (line_end + 1).min(input.len());
            continue;
        }
        let indent = line.len() - line.trim_start_matches(' ').len();
        if !indent.is_multiple_of(2) || !line.trim_start_matches(' ').starts_with("uid=") {
            return None;
        }
        let mut pos = start + indent + 4;
        let uid_end = input[pos..]
            .find(char::is_whitespace)
            .map(|i| pos + i)
            .unwrap_or(input.len())
            .min(line_end);
        let uid = &input[pos..uid_end];
        if uid.is_empty() {
            return None;
        }
        pos = uid_end;

        let mut role = String::new();
        let mut name = String::new();
        let mut has_name = false;
        if input[pos..].starts_with(" \"") {
            let (inner, next) = scan_double_quoted(&input[pos + 1..])?;
            name = inner;
            has_name = true;
            pos += 1 + next;
        } else if input[pos..].starts_with(' ') {
            let word_start = pos + 1;
            let word_end = input[word_start..]
                .find(|c: char| !(c.is_alphanumeric() || c == '_' || c == '-'))
                .map(|i| word_start + i)
                .unwrap_or(input.len())
                .min(line_end);
            if word_end > word_start {
                role = input[word_start..word_end].to_owned();
                pos = word_end;
                if input[pos..].starts_with(" \"") {
                    let (inner, next) = scan_double_quoted(&input[pos + 1..])?;
                    name = inner;
                    has_name = true;
                    pos += 1 + next;
                }
            }
        }

        let rest_line_end = input[pos..]
            .find('\n')
            .map(|i| pos + i)
            .unwrap_or(input.len());
        let rest_raw = &input[pos..rest_line_end];
        let raw_span = &input[start..rest_line_end];

        let mut rebuilt = " ".repeat(indent);
        rebuilt.push_str("uid=");
        rebuilt.push_str(uid);
        if !role.is_empty() {
            rebuilt.push(' ');
            rebuilt.push_str(&role);
        }
        if has_name {
            rebuilt.push_str(" \"");
            rebuilt.push_str(&name);
            rebuilt.push('"');
        }
        rebuilt.push_str(rest_raw);
        if rebuilt != raw_span {
            return None;
        }

        let (url, value, attrs) = decompose_attrs(rest_raw)?;
        records.push(Record {
            depth: indent / 2,
            role,
            name,
            reference: uid.to_owned(),
            attrs,
            url,
            value,
            text: String::new(),
        });
        raws.push(raw_span.to_owned());
        li = line_starts.partition_point(|&s| s <= rest_line_end);
        consumed_end = (rest_line_end + 1).min(input.len());
    }

    let suffix = input[consumed_end..].to_owned();
    if format!("{prefix}{}{suffix}", raws.join("\n")) != input {
        return None;
    }
    Some(Parsed {
        format: Format::ChromeDevTools,
        prefix,
        suffix,
        records,
    })
}

fn line_start_offsets(input: &str) -> Vec<usize> {
    let mut starts = vec![0];
    for (i, c) in input.char_indices() {
        if c == '\n' {
            starts.push(i + 1);
        }
    }
    starts
}

/// Splits the attribute tail into url, value, and remaining words.
///
/// `url` is normalized to its bare value (quoted and bare forms are
/// semantically identical); `value` keeps its raw token including quotes so
/// an explicit empty `value=""` stays distinguishable from an absent value.
/// Duplicate url/value tokens are ambiguous and reject the whole snapshot.
fn decompose_attrs(rest_raw: &str) -> Option<(String, String, String)> {
    let mut url: Option<String> = None;
    let mut value: Option<String> = None;
    let mut attrs: Vec<String> = Vec::new();
    for token in attr_tokens(rest_raw.trim())? {
        if let Some(v) = token.strip_prefix("url=") {
            if url.is_some() {
                return None;
            }
            url = Some(unquote(v).to_owned());
        } else if let Some(v) = token.strip_prefix("value=") {
            if value.is_some() {
                return None;
            }
            value = Some(v.to_owned());
        } else {
            attrs.push(token);
        }
    }
    Some((
        url.unwrap_or_default(),
        value.unwrap_or_default(),
        attrs.join(" "),
    ))
}

fn unquote(v: &str) -> &str {
    if v.len() >= 2 && v.starts_with('"') && v.ends_with('"') {
        &v[1..v.len() - 1]
    } else {
        v
    }
}

/// Whitespace-separated attribute words; quoted scalars stay inside their
/// word so `description="see url= x"` cannot be mistaken for a url token.
fn attr_tokens(s: &str) -> Option<Vec<String>> {
    let mut tokens = Vec::new();
    let mut pos = 0;
    while pos < s.len() {
        let c = s[pos..].chars().next()?;
        if c.is_whitespace() {
            pos += c.len_utf8();
            continue;
        }
        let mut token = String::new();
        while pos < s.len() {
            let c = s[pos..].chars().next()?;
            if c.is_whitespace() {
                break;
            }
            if c == '"' {
                let (inner, next) = scan_double_quoted(&s[pos..])?;
                token.push('"');
                token.push_str(&inner);
                token.push('"');
                pos += next;
            } else {
                token.push(c);
                pos += c.len_utf8();
            }
        }
        tokens.push(token);
    }
    Some(tokens)
}

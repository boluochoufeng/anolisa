//! Lossless re-encoding of browser accessibility-tree snapshots.
//!
//! Two subformats share one record model: Playwright MCP's fenced YAML aria
//! tree (`- role "name" [ref=eN]`) and chrome-devtools-mcp's flat uid listing
//! (`uid=N_M role "name"`). The compressor flattens the tree into a record
//! table, drops redundant cursor annotations on interactive roles, and shares
//! repeated URL prefixes behind a legend. Every element reference, role/name
//! binding, URL, and text byte is retained; the Runtime stashes the original
//! snapshot for retrieval. Any parse or fidelity failure returns `None` so the
//! caller passes the input through unchanged.

mod cdt;
mod encode;
mod pw;

use tokenless_protocol::estimate_tokens;

/// Flattened accessibility-tree node. `reference` holds the Playwright `ref`
/// or the chrome-devtools `uid`; `text` carries Playwright node content and
/// `value` carries chrome-devtools input values.
#[derive(Clone, PartialEq)]
pub(crate) struct Record {
    pub depth: usize,
    pub role: String,
    pub name: String,
    pub reference: String,
    pub attrs: String,
    pub url: String,
    pub value: String,
    pub text: String,
}

/// Snapshot subformat recognized by the authoritative parsers.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Format {
    Playwright,
    ChromeDevTools,
}

/// Verbatim wrapper pieces plus the flattened tree.
pub(crate) struct Parsed {
    pub format: Format,
    pub prefix: String,
    pub suffix: String,
    pub records: Vec<Record>,
}

/// Scans a double-quoted scalar from the start of `s`, backslash-aware.
///
/// Returns the raw inner bytes (escapes and embedded newlines preserved) and
/// the byte index just past the closing quote.
pub(crate) fn scan_double_quoted(s: &str) -> Option<(String, usize)> {
    let mut chars = s.char_indices();
    if chars.next()?.1 != '"' {
        return None;
    }
    let mut out = String::new();
    while let Some((i, c)) = chars.next() {
        match c {
            '\\' => {
                let (_, next) = chars.next()?;
                out.push('\\');
                out.push(next);
            }
            '"' => return Some((out, i + 1)),
            _ => out.push(c),
        }
    }
    None
}

/// A diff of a snapshot file carries snapshot anchors in its context rows;
/// the diff domain owns those, so both parsers reject diff-shaped wrappers.
pub(crate) fn is_diff_wrapper(prefix: &str) -> bool {
    prefix.contains("diff --git ")
        || prefix.starts_with("--- ")
        || prefix.starts_with("+++ ")
        || prefix.starts_with("@@ ")
}

/// Stateless compressor for browser accessibility snapshots.
pub struct BrowserSnapshotCompressor;

impl BrowserSnapshotCompressor {
    /// Returns a lossless record-table view, or `None` to keep the input.
    ///
    /// Fails open: unrecognized shapes, parse errors, reconstruction
    /// mismatches, and candidates without a net byte-and-token benefit all
    /// return `None`.
    #[must_use]
    pub fn compress(&self, input: &str) -> Option<String> {
        let parsed = pw::parse(input).or_else(|| cdt::parse(input))?;
        let original = parsed.records;
        let mut records = original.clone();
        if parsed.format == Format::Playwright {
            encode::strip_redundant_cursor(&mut records);
        }
        let cols: &[encode::Col] = match parsed.format {
            Format::Playwright => encode::PW_COLS,
            Format::ChromeDevTools => encode::CDT_COLS,
        };
        let mut table = encode::table(&records, cols);
        let mut legend_text = String::new();
        let mut legend: Vec<(String, String)> = Vec::new();
        if let Some(shared) = encode::share_urls(&records) {
            let shared_table = encode::table(&shared.records, cols);
            let candidate = format!("{}\n{shared_table}", shared.legend_text);
            if candidate.len() < table.len()
                && estimate_tokens(&candidate) < estimate_tokens(&table)
            {
                table = shared_table;
                legend_text = shared.legend_text;
                legend = shared.legend;
                records = shared.records;
            }
        }
        if !encode::roundtrip_ok(&original, &records, &table, cols, &legend) {
            return None;
        }
        let output = if legend_text.is_empty() {
            format!("{}{table}{}", parsed.prefix, parsed.suffix)
        } else {
            format!("{}{legend_text}\n{table}{}", parsed.prefix, parsed.suffix)
        };
        (output.len() < input.len() && estimate_tokens(&output) < estimate_tokens(input))
            .then_some(output)
    }
}

#[cfg(test)]
mod tests {
    include!("tests/browser_snapshot_tests.rs");
}

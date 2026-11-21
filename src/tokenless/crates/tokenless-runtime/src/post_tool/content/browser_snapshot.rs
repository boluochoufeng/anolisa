//! Browser accessibility-snapshot shape detection.
//!
//! Runs first in the domain chain: snapshots can embed diff, test, or source
//! markers that later detectors would claim, while `[ref=` / `uid=` line
//! anchors are unique to snapshots. Shape only — the compressor parsers are
//! authoritative and fail open.

/// Non-empty lines inspected by the ratio fallbacks.
const MAX_RATIO_LINES: usize = 50;

/// True for Playwright MCP aria snapshots or chrome-devtools-mcp uid listings.
pub(super) fn is_browser_snapshot(scan: &str) -> bool {
    if looks_like_diff(scan) {
        return false;
    }
    is_playwright(scan) || is_chrome_devtools(scan)
}

/// A diff of a snapshot file carries the same line anchors in its context
/// rows; the diff domain must keep owning those. Head-anchored so a snapshot
/// merely quoting diff text in a node name is not rejected; the compressor
/// parsers keep a contains-based guard for diffs with a command preamble.
fn looks_like_diff(scan: &str) -> bool {
    scan.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .is_some_and(|first| {
            first.starts_with("diff --git ")
                || first.starts_with("--- ")
                || first.starts_with("+++ ")
                || first.starts_with("@@ ")
        })
}

fn sampled_lines(scan: &str) -> Vec<&str> {
    scan.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .take(MAX_RATIO_LINES)
        .collect()
}

fn is_playwright(scan: &str) -> bool {
    let lines = sampled_lines(scan);
    let Some(first) = lines.first() else {
        return false;
    };
    if first.starts_with("### Page") && scan.contains("```yaml") && scan.contains("[ref=") {
        return true;
    }
    // Bare snapshot (e.g. a saved .yml body quoted into a tool result). The
    // first line must itself be a tree row so command preambles (a diff
    // echoed after its shell command) never reach the ratio fallback.
    if lines.len() < 3 || !is_pw_line(first) {
        return false;
    }
    let pw_lines = lines.iter().filter(|l| is_pw_line(l)).count();
    let refs = lines.iter().filter(|l| ref_value(l).is_some()).count();
    refs >= 3 && pw_lines * 5 >= lines.len() * 4
}

fn is_pw_line(line: &str) -> bool {
    line.starts_with("- ")
        && (line.contains("[ref=") || line.starts_with("- /url: ") || line.starts_with("- text: "))
}

/// Extracts a well-formed `eN` or `fNeM` element reference, if present.
fn ref_value(line: &str) -> Option<&str> {
    let start = line.find("[ref=")? + "[ref=".len();
    let end = start + line[start..].find(']')?;
    let value = &line[start..end];
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    if let Some(rest) = value.strip_prefix('e') {
        return digits(rest).then_some(value);
    }
    let (frame, element) = value.strip_prefix('f')?.split_once('e')?;
    (digits(frame) && digits(element)).then_some(value)
}

fn is_chrome_devtools(scan: &str) -> bool {
    let lines = sampled_lines(scan);
    if let Some(pos) = lines
        .iter()
        .take(10)
        .position(|l| *l == "## Latest page snapshot")
        && lines.get(pos + 1).is_some_and(|l| is_uid_line(l))
    {
        return true;
    }
    if lines.len() < 3 || !lines.first().is_some_and(|l| is_uid_line(l)) {
        return false;
    }
    let uid_lines = lines.iter().filter(|l| is_uid_line(l)).count();
    uid_lines >= 3 && uid_lines * 5 >= lines.len() * 4
}

fn is_uid_line(line: &str) -> bool {
    let Some(rest) = line.strip_prefix("uid=") else {
        return false;
    };
    let token_end = rest.find(char::is_whitespace).unwrap_or(rest.len());
    let Some((generation, sequence)) = rest[..token_end].split_once('_') else {
        return false;
    };
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    digits(generation)
        && digits(sequence)
        && matches!(rest[token_end..].chars().next(), None | Some(' '))
}

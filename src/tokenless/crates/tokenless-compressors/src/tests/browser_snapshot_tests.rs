use super::encode::{self, Col};
use super::{BrowserSnapshotCompressor, Record};
use tokenless_protocol::estimate_tokens;

const PW_WRAP: &str = "### Page\n- Page URL: https://example.com/\n- Page Title: Example\n### Snapshot\n";

fn pw_input(tree: &str) -> String {
    format!("{PW_WRAP}```yaml\n{tree}\n```")
}

fn cdt_input(body: &str) -> String {
    format!("## Latest page snapshot\n{body}")
}

fn decode_table(table: &str, cols: &[Col]) -> Vec<Record> {
    let mut lines = table.split('\n');
    lines.next().unwrap();
    lines
        .map(|line| {
            let cells: Vec<String> = line.split('\t').map(encode::unescape_cell).collect();
            let mut record = Record {
                depth: 0,
                role: String::new(),
                name: String::new(),
                reference: String::new(),
                attrs: String::new(),
                url: String::new(),
                value: String::new(),
                text: String::new(),
            };
            for (col, cell) in cols.iter().zip(&cells) {
                match col {
                    Col::Depth => record.depth = cell.parse().unwrap(),
                    Col::Role => record.role = cell.clone(),
                    Col::Name => record.name = cell.clone(),
                    Col::Ref | Col::Uid => record.reference = cell.clone(),
                    Col::Attrs => record.attrs = cell.clone(),
                    Col::Url => record.url = cell.clone(),
                    Col::Value => record.value = cell.clone(),
                    Col::Text => record.text = cell.clone(),
                }
            }
            record
        })
        .collect()
}

// ---- Playwright parser edges ----

#[test]
fn pw_parses_tree_with_url_children_and_text_nodes() {
    let input = pw_input(
        "- generic [ref=e1]:\n  - link \"Home\" [ref=e2] [cursor=pointer]:\n    - /url: https://example.com/\n  - paragraph [ref=e3]: Hello\n  - text: world",
    );
    let parsed = super::pw::parse(&input).unwrap();
    assert_eq!(parsed.prefix, PW_WRAP);
    assert_eq!(parsed.suffix, "");
    assert_eq!(parsed.records.len(), 4);
    assert_eq!(parsed.records[1].reference, "e2");
    assert_eq!(parsed.records[1].url, "https://example.com/");
    assert_eq!(parsed.records[1].attrs, "cursor=pointer");
    assert_eq!(parsed.records[2].text, "Hello");
    assert_eq!(parsed.records[3].role, "text");
    assert_eq!(parsed.records[3].text, "world");
}

#[test]
fn pw_single_quoted_scalar_with_colon_in_name() {
    let input = pw_input(
        "- 'link \"Hurried Announcement: Alessandro\" [ref=e40] [cursor=pointer]':\n  - /url: https://example.com/a",
    );
    let parsed = super::pw::parse(&input).unwrap();
    assert_eq!(parsed.records[0].name, "Hurried Announcement: Alessandro");
    assert_eq!(parsed.records[0].reference, "e40");
    assert_eq!(parsed.records[0].url, "https://example.com/a");
}

#[test]
fn pw_multiline_quoted_name_folds_to_space() {
    let input = pw_input("- paragraph \"first\n  second\" [ref=e7]:\n- link \"Next\" [ref=e8]");
    let parsed = super::pw::parse(&input).unwrap();
    assert_eq!(parsed.records.len(), 2);
    assert!(parsed.records[0].name.contains("first"));
    assert!(parsed.records[0].name.contains("second"));
}

#[test]
fn pw_frame_refs_and_variable_attribute_order() {
    let input = pw_input("- generic [active] [ref=f2e2] [box=0,0,1280,720]:\n  - textbox \"q\" [active] [ref=f2e38]");
    let parsed = super::pw::parse(&input).unwrap();
    assert_eq!(parsed.records[0].reference, "f2e2");
    assert_eq!(parsed.records[0].attrs, "active box=0,0,1280,720");
    assert_eq!(parsed.records[1].reference, "f2e38");
    assert_eq!(parsed.records[1].attrs, "active");
}

#[test]
fn pw_orphan_url_and_bad_indent_reject_whole_snapshot() {
    assert!(super::pw::parse(&pw_input("- /url: https://example.com/")).is_none());
    assert!(super::pw::parse(&pw_input("- generic [ref=e1]:\n   - link \"x\" [ref=e2]")).is_none());
    assert!(super::pw::parse(&pw_input("- generic [ref=e1]:\n\t- link \"x\" [ref=e2]")).is_none());
    assert!(super::pw::parse(&pw_input("- generic [ref=e1]\n  - /url: https://x/")).is_some());
    assert!(super::pw::parse(&pw_input("- generic [ref=e1]\n  - /url: https://x/\n  - /url: https://y/")).is_none());
}

#[test]
fn pw_trailer_after_fence_is_preserved() {
    let input = format!(
        "{PW_WRAP}```yaml\n- link \"Home\" [ref=e2]\n```\n### Events\n- New console entries: file.log#L1-L2"
    );
    let parsed = super::pw::parse(&input).unwrap();
    assert_eq!(parsed.suffix, "\n### Events\n- New console entries: file.log#L1-L2");
}

// ---- chrome-devtools parser edges ----

#[test]
fn cdt_parses_roles_names_urls_and_roleless_text() {
    let input = cdt_input(
        "uid=1_0 RootWebArea \"Example\" url=\"https://example.com/\"\n  uid=1_1 link \"Home\" url=\"https://example.com/\"\n  uid=1_6 \" | \"\n  uid=1_2 button \"Go\" focusable focused",
    );
    let parsed = super::cdt::parse(&input).unwrap();
    assert_eq!(parsed.records.len(), 4);
    assert_eq!(parsed.records[0].role, "RootWebArea");
    assert_eq!(parsed.records[1].url, "https://example.com/");
    assert_eq!(parsed.records[2].role, "");
    assert_eq!(parsed.records[2].name, " | ");
    assert_eq!(parsed.records[3].attrs, "focusable focused");
}

#[test]
fn cdt_multiline_raw_newline_name_survives_reconstruction() {
    let input = cdt_input("uid=4_10 StaticText \"line1\nline2\nline3\"\nuid=4_11 link \"Next\"");
    let parsed = super::cdt::parse(&input).unwrap();
    assert_eq!(parsed.records.len(), 2);
    assert_eq!(parsed.records[0].name, "line1\nline2\nline3");
    assert_eq!(parsed.records[1].reference, "4_11");
}

#[test]
fn cdt_action_prefix_and_verbose_ignored_nodes() {
    let input = "Successfully clicked on the element\nPage navigated to https://example.com/next.\n## Latest page snapshot\nuid=2_0 RootWebArea \"Next\" busy url=\"https://example.com/next\"\n  uid=2_1 ignored\n  uid=2_2 link \"Home\" url=\"https://example.com/\"";
    let parsed = super::cdt::parse(input).unwrap();
    assert!(parsed.prefix.starts_with("Successfully clicked"));
    assert!(parsed.prefix.ends_with("## Latest page snapshot\n"));
    assert_eq!(parsed.records[1].role, "ignored");
    assert_eq!(parsed.records[0].attrs, "busy");
}

#[test]
fn cdt_value_empty_stays_distinct_from_absent_and_bare_urls_normalize() {
    let input = cdt_input(
        "uid=1_1 textbox \"q\" value=\"\"\nuid=1_2 textbox \"r\"\nuid=1_3 link \"js\" url=javascript:;",
    );
    let parsed = super::cdt::parse(&input).unwrap();
    assert_eq!(parsed.records[0].value, "\"\"");
    assert_eq!(parsed.records[1].value, "");
    assert_eq!(parsed.records[2].url, "javascript:;");
}

#[test]
fn cdt_quoted_description_does_not_leak_url_and_duplicates_reject() {
    let input = cdt_input("uid=1_1 img \"logo\" description=\"see url= x\" url=\"https://example.com/l.png\"");
    let parsed = super::cdt::parse(&input).unwrap();
    assert_eq!(parsed.records[0].url, "https://example.com/l.png");
    assert!(parsed.records[0].attrs.contains("description=\"see url= x\""));
    let dup = cdt_input("uid=1_1 link \"x\" url=\"https://a/\" url=\"https://b/\"");
    assert!(super::cdt::parse(&dup).is_none());
}

#[test]
fn cdt_unterminated_quote_and_bad_indent_reject() {
    assert!(super::cdt::parse(&cdt_input("uid=1_1 StaticText \"open")).is_none());
    assert!(super::cdt::parse(&cdt_input("uid=1_0 RootWebArea \"x\"\n   uid=1_1 link \"y\"")).is_none());
}

// ---- encoding transforms ----

#[test]
fn smart_cursor_prunes_interactive_roles_only() {
    let mut records = vec![
        Record { depth: 0, role: "link".into(), name: "a".into(), reference: "e1".into(), attrs: "cursor=pointer".into(), url: String::new(), value: String::new(), text: String::new() },
        Record { depth: 0, role: "generic".into(), name: String::new(), reference: "e2".into(), attrs: "cursor=pointer".into(), url: String::new(), value: String::new(), text: String::new() },
    ];
    encode::strip_redundant_cursor(&mut records);
    assert_eq!(records[0].attrs, "");
    assert_eq!(records[1].attrs, "cursor=pointer");
}

#[test]
fn escape_roundtrip_handles_backslash_tab_newline_and_quotes() {
    for raw in ["a\\b", "a\tb", "a\nb", "a\\nb", "\"q\"", "中文\\n"] {
        assert_eq!(encode::unescape_cell(&encode::escape_cell(raw)), raw);
    }
}

#[test]
fn url_share_legend_encodes_and_decodes_exactly() {
    let records: Vec<Record> = (0..6)
        .map(|i| Record {
            depth: 1,
            role: "link".into(),
            name: format!("item {i}"),
            reference: format!("e{i}"),
            attrs: String::new(),
            url: format!("https://example.com/items/item-{i}"),
            value: String::new(),
            text: String::new(),
        })
        .collect();
    let shared = encode::share_urls(&records).unwrap();
    assert_eq!(shared.legend_text, "U1=https://example.com/items/");
    assert_eq!(shared.records[0].url, "U1/item-0");
    for (orig, enc) in records.iter().zip(&shared.records) {
        assert_eq!(encode::decode_url(&enc.url, &shared.legend), orig.url);
    }
}

#[test]
fn url_share_collision_guard_disables_legend() {
    let mut records: Vec<Record> = (0..4)
        .map(|i| Record {
            depth: 1, role: "link".into(), name: format!("n{i}"), reference: format!("e{i}"),
            attrs: String::new(), url: format!("https://example.com/items/{i}"),
            value: String::new(), text: String::new(),
        })
        .collect();
    records[0].url = "U1/evil".to_owned();
    assert!(encode::share_urls(&records).is_none());
}

// ---- end-to-end compress ----

fn pw_links(count: usize) -> String {
    let mut tree = String::from("- navigation [ref=e1]:\n");
    for i in 0..count {
        tree.push_str(&format!(
            "  - link \"Story number {i} with a reasonably long title\" [ref=e{}] [cursor=pointer]:\n    - /url: https://news.example.com/item?id={i}\n",
            i + 2
        ));
    }
    pw_input(&tree)
}

#[test]
fn compress_pw_yields_lossless_table_with_wrapper_and_legend() {
    let input = pw_links(40);
    let output = BrowserSnapshotCompressor.compress(&input).unwrap();
    assert!(output.starts_with(PW_WRAP));
    assert!(output.contains("U1=https://news.example.com/"));
    assert!(output.len() < input.len());
    assert!(estimate_tokens(&output) < estimate_tokens(&input));
    let body = output.strip_prefix(PW_WRAP).unwrap();
    let (legend_text, table) = body.split_once('\n').unwrap();
    let legend: Vec<(String, String)> = legend_text
        .split('\n')
        .filter(|l| l.starts_with("U1="))
        .map(|l| { let (id, p) = l.split_once('=').unwrap(); (id.to_owned(), p.to_owned()) })
        .collect();
    let decoded = decode_table(table, encode::PW_COLS);
    assert_eq!(decoded.len(), 41);
    assert_eq!(decoded[1].reference, "e2");
    assert_eq!(decoded[1].attrs, "");
    assert_eq!(
        encode::decode_url(&decoded[1].url, &legend),
        "https://news.example.com/item?id=0"
    );
    assert_eq!(decoded[1].name, "Story number 0 with a reasonably long title");
}

#[test]
fn compress_cdt_yields_lossless_table_with_uids() {
    let mut body = String::from("uid=1_0 RootWebArea \"News\" url=\"https://news.example.com/\"\n");
    for i in 0..40 {
        body.push_str(&format!(
            "  uid=1_{} link \"Story number {i} with a reasonably long title\" url=\"https://news.example.com/item?id={i}\"\n",
            i + 1
        ));
    }
    let input = cdt_input(&body);
    let output = BrowserSnapshotCompressor.compress(&input).unwrap();
    assert!(output.starts_with("## Latest page snapshot\n"));
    assert!(output.len() < input.len());
    let body_out = output.strip_prefix("## Latest page snapshot\n").unwrap();
    let table = match body_out.split_once('\n') {
        Some((first, rest)) if first.starts_with("U1=") => rest,
        _ => body_out,
    };
    let decoded = decode_table(table, encode::CDT_COLS);
    assert_eq!(decoded.len(), 41);
    assert_eq!(decoded[1].reference, "1_1");
    assert_eq!(decoded[1].role, "link");
}

#[test]
fn compress_fails_open_on_unrelated_or_tiny_input() {
    for input in [
        "",
        "just some prose about the web",
        "{\"json\": true}",
        &pw_input("- link \"x\" [ref=e1]"),
        &cdt_input("uid=1_1 link \"x\""),
    ] {
        assert!(BrowserSnapshotCompressor.compress(input).is_none(), "{input:?}");
    }
}

#[test]
fn compress_synthetic_large_tree_roundtrips_every_record() {
    let mut tree = String::from("- generic [ref=e1]:\n");
    for i in 0..500 {
        let name = format!("node {i} 中文 \\\\ back\\ttab");
        tree.push_str(&format!(
            "  - link \"{name}\" [ref=e{}] [cursor=pointer]:\n    - /url: https://example.com/deep/path/item-{i}\n",
            i + 2
        ));
    }
    let input = pw_input(&tree);
    let output = BrowserSnapshotCompressor.compress(&input).unwrap();
    let parsed = super::pw::parse(&input).unwrap();
    let body = output.strip_prefix(PW_WRAP).unwrap();
    let table = match body.split_once('\n') {
        Some((first, rest)) if first.starts_with("U1=") => rest,
        _ => body,
    };
    let decoded = decode_table(table, encode::PW_COLS);
    assert_eq!(decoded.len(), parsed.records.len());
    for (orig, dec) in parsed.records.iter().zip(&decoded) {
        assert_eq!(orig.depth, dec.depth);
        assert_eq!(orig.role, dec.role);
        assert_eq!(orig.name, dec.name);
        assert_eq!(orig.reference, dec.reference);
        assert_eq!(orig.text, dec.text);
    }
}

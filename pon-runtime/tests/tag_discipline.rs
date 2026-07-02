use std::fs;
use std::path::{Path, PathBuf};

#[test]
fn extern_pon_helpers_with_object_params_start_with_untag_prelude() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let mut files = Vec::new();
    collect_rs_files(&manifest.join("src"), &mut files);
    files.sort();
    files.dedup();

    let mut offenders = Vec::new();
    for file in files {
        let source = fs::read_to_string(&file).expect("read runtime source");
        for helper in helpers_in(&source) {
            if helper.pyobject_params.is_empty() || helper.has_tag_ok_marker {
                continue;
            }
            if !helper
                .first_statement
                .is_some_and(|statement| statement.starts_with("crate::untag_prelude!"))
            {
                let rel = file.strip_prefix(&manifest).unwrap_or(&file);
                offenders.push(format!(
                    "{}:{} {}({}) first statement {:?}",
                    rel.display(),
                    helper.line,
                    helper.name,
                    helper.pyobject_params.join(", "),
                    helper.first_statement,
                ));
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "tagged-int helper prelude audit failed:\n{}",
        offenders.join("\n"),
    );
}

fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries {
        let path = entry.expect("read source dir entry").path();
        if path.is_dir() {
            collect_rs_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

#[derive(Debug)]
struct Helper<'a> {
    name: &'a str,
    line: usize,
    pyobject_params: Vec<&'a str>,
    first_statement: Option<&'a str>,
    has_tag_ok_marker: bool,
}

fn helpers_in(source: &str) -> Vec<Helper<'_>> {
    const NEEDLE: &str = "pub unsafe extern \"C\" fn pon_";
    let mut helpers = Vec::new();
    let mut search_from = 0;
    while let Some(relative) = source[search_from..].find(NEEDLE) {
        let start = search_from + relative;
        let name_start = start + "pub unsafe extern \"C\" fn ".len();
        let Some(open_paren_rel) = source[name_start..].find('(') else {
            break;
        };
        let open_paren = name_start + open_paren_rel;
        let name = &source[name_start..open_paren];
        let Some(close_paren) = matching_delimiter(source, open_paren, '(', ')') else {
            search_from = open_paren + 1;
            continue;
        };
        let Some(open_brace_rel) = source[close_paren..].find('{') else {
            search_from = close_paren + 1;
            continue;
        };
        let open_brace = close_paren + open_brace_rel;
        let Some(close_brace) = matching_delimiter(source, open_brace, '{', '}') else {
            search_from = open_brace + 1;
            continue;
        };
        let params = &source[open_paren + 1..close_paren];
        let body = &source[open_brace + 1..close_brace];
        helpers.push(Helper {
            name,
            line: 1 + source[..start].bytes().filter(|&byte| byte == b'\n').count(),
            pyobject_params: direct_pyobject_params(params),
            first_statement: first_statement(body),
            has_tag_ok_marker: has_tag_ok_marker(source, start, body),
        });
        search_from = close_brace + 1;
    }
    helpers
}

fn direct_pyobject_params(params: &str) -> Vec<&str> {
    split_top_level_commas(params)
        .into_iter()
        .filter_map(|param| {
            let (name, ty) = param.split_once(':')?;
            let ty = normalize_ws(ty);
            if ty == "*mut PyObject" {
                Some(name.trim().trim_start_matches("mut ").trim())
            } else {
                None
            }
        })
        .collect()
}

fn first_statement(body: &str) -> Option<&str> {
    body.lines().map(str::trim).find(|line| {
        !line.is_empty()
            && !line.starts_with("//")
            && !line.starts_with("#")
            && !line.starts_with("/*")
            && !line.starts_with("*")
    })
}

fn has_tag_ok_marker(source: &str, fn_start: usize, body: &str) -> bool {
    let prefix_start = source[..fn_start]
        .char_indices()
        .rev()
        .nth(256)
        .map_or(0, |(index, _)| index);
    source[prefix_start..fn_start].contains("TAG-OK:")
        || body
            .lines()
            .map(str::trim)
            .take_while(|line| line.is_empty() || line.starts_with("//"))
            .any(|line| line.contains("TAG-OK:"))
}

fn split_top_level_commas(input: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut depth = 0usize;
    let mut start = 0usize;
    for (index, ch) in input.char_indices() {
        match ch {
            '(' | '<' | '[' => depth += 1,
            ')' | '>' | ']' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                let part = input[start..index].trim();
                if !part.is_empty() {
                    parts.push(part);
                }
                start = index + ch.len_utf8();
            }
            _ => {}
        }
    }
    let tail = input[start..].trim();
    if !tail.is_empty() {
        parts.push(tail);
    }
    parts
}

fn normalize_ws(input: &str) -> String {
    input.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn matching_delimiter(source: &str, open: usize, opener: char, closer: char) -> Option<usize> {
    let mut depth = 0usize;
    let mut in_line_comment = false;
    let mut block_comment_depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    let mut iter = source[open..].char_indices().peekable();
    while let Some((relative, ch)) = iter.next() {
        let absolute = open + relative;
        let next = iter.peek().map(|(_, c)| *c);
        if in_line_comment {
            if ch == '\n' {
                in_line_comment = false;
            }
            continue;
        }
        if block_comment_depth > 0 {
            if ch == '/' && next == Some('*') {
                block_comment_depth += 1;
                iter.next();
            } else if ch == '*' && next == Some('/') {
                block_comment_depth -= 1;
                iter.next();
            }
            continue;
        }
        if in_string {
            if escaped {
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        if ch == '/' && next == Some('/') {
            in_line_comment = true;
            iter.next();
            continue;
        }
        if ch == '/' && next == Some('*') {
            block_comment_depth = 1;
            iter.next();
            continue;
        }
        if ch == '"' {
            in_string = true;
            continue;
        }
        if ch == opener {
            depth += 1;
        } else if ch == closer {
            depth = depth.checked_sub(1)?;
            if depth == 0 {
                return Some(absolute);
            }
        }
    }
    None
}

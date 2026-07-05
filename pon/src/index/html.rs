//! Minimal parser for legacy PEP 503 simple-index HTML pages.

use std::collections::BTreeMap;

use crate::error::{Error, Result};
use crate::names;

use super::simple_json::project_file_from_parts;
use super::{DistInfoMetadata, ProjectPage};

/// Parse a PEP 503 project page into the same `ProjectPage` shape as PEP 691 JSON.
pub fn parse_project_html(base_url: &str, body: &str) -> Result<Option<ProjectPage>> {
    let name = project_name_from_base_url(base_url)?;
    let mut files = Vec::new();
    let mut offset = 0;

    while let Some(anchor) = next_anchor(body, offset) {
        offset = anchor.next_offset;
        let attrs = parse_attrs(anchor.attrs);
        let Some(href) = attrs.get("href") else {
            continue;
        };
        let filename = html_unescape(anchor.text).trim().to_owned();
        if filename.is_empty() {
            continue;
        }

        let resolved = resolve_href(base_url, href);
        let (url, hashes) = strip_hash_fragment(&resolved);
        let requires_python = attrs.get("data-requires-python").cloned();
        let yanked = attrs.get("data-yanked").cloned();
        let dist_info_metadata = metadata_from_attrs(&attrs);

        if let Some(file) = project_file_from_parts(
            filename,
            url,
            hashes,
            requires_python,
            yanked,
            dist_info_metadata,
        ) {
            files.push(file);
        }
    }

    Ok(Some(ProjectPage {
        meta_api_version: "1.0".to_owned(),
        name,
        files,
    }))
}

struct Anchor<'a> {
    attrs: &'a str,
    text: &'a str,
    next_offset: usize,
}

fn next_anchor(body: &str, mut offset: usize) -> Option<Anchor<'_>> {
    while offset < body.len() {
        let start = offset + find_ascii_case_insensitive(&body[offset..], "<a")?;
        let after_name = body[start + 2..].chars().next();
        if !matches!(after_name, Some(ch) if ch == '>' || ch == '/' || ch.is_ascii_whitespace()) {
            offset = start + 2;
            continue;
        }

        let tag_end = start + body[start..].find('>')?;
        let text_start = tag_end + 1;
        let close_rel = find_ascii_case_insensitive(&body[text_start..], "</a>")?;
        let text_end = text_start + close_rel;
        return Some(Anchor {
            attrs: &body[start + 2..tag_end],
            text: &body[text_start..text_end],
            next_offset: text_end + "</a>".len(),
        });
    }
    None
}

fn parse_attrs(source: &str) -> BTreeMap<String, String> {
    let mut attrs = BTreeMap::new();
    let bytes = source.as_bytes();
    let mut index = 0;

    while index < bytes.len() {
        while index < bytes.len() && (bytes[index].is_ascii_whitespace() || bytes[index] == b'/') {
            index += 1;
        }
        if index >= bytes.len() {
            break;
        }

        let name_start = index;
        while index < bytes.len() && is_attr_name_byte(bytes[index]) {
            index += 1;
        }
        if name_start == index {
            index += 1;
            continue;
        }
        let name = source[name_start..index].to_ascii_lowercase();

        while index < bytes.len() && bytes[index].is_ascii_whitespace() {
            index += 1;
        }

        let value = if index < bytes.len() && bytes[index] == b'=' {
            index += 1;
            while index < bytes.len() && bytes[index].is_ascii_whitespace() {
                index += 1;
            }
            if index < bytes.len() && (bytes[index] == b'\'' || bytes[index] == b'"') {
                let quote = bytes[index];
                index += 1;
                let value_start = index;
                while index < bytes.len() && bytes[index] != quote {
                    index += 1;
                }
                let value = html_unescape(&source[value_start..index]);
                if index < bytes.len() {
                    index += 1;
                }
                value
            } else {
                let value_start = index;
                while index < bytes.len() && !bytes[index].is_ascii_whitespace() {
                    index += 1;
                }
                html_unescape(&source[value_start..index])
            }
        } else {
            String::new()
        };

        attrs.insert(name, value);
    }

    attrs
}

fn metadata_from_attrs(attrs: &BTreeMap<String, String>) -> Option<DistInfoMetadata> {
    attrs
        .get("data-core-metadata")
        .or_else(|| attrs.get("data-dist-info-metadata"))
        .and_then(|value| metadata_from_attr_value(value))
}

fn metadata_from_attr_value(value: &str) -> Option<DistInfoMetadata> {
    let value = value.trim();
    if value.eq_ignore_ascii_case("false") {
        return None;
    }

    let mut hashes = BTreeMap::new();
    if let Some((algorithm, digest)) = value.split_once('=') {
        let algorithm = algorithm.trim();
        let digest = digest.trim();
        if !algorithm.is_empty() && !digest.is_empty() {
            hashes.insert(algorithm.to_ascii_lowercase(), digest.to_owned());
        }
    }
    Some(DistInfoMetadata { hashes })
}

fn project_name_from_base_url(base_url: &str) -> Result<String> {
    let without_fragment = base_url.split_once('#').map_or(base_url, |(url, _)| url);
    let without_query = without_fragment
        .split_once('?')
        .map_or(without_fragment, |(url, _)| url);
    let Some(raw_name) = without_query.trim_end_matches('/').rsplit('/').next() else {
        return Err(Error::Index(format!(
            "simple HTML project URL `{base_url}` has no project name"
        )));
    };
    if raw_name.is_empty() {
        return Err(Error::Index(format!(
            "simple HTML project URL `{base_url}` has no project name"
        )));
    }
    names::validate(raw_name)?;
    Ok(names::normalize(raw_name))
}

fn resolve_href(base_url: &str, href: &str) -> String {
    let href = href.trim();
    if has_scheme(href) {
        return href.to_owned();
    }
    if let Some(rest) = href.strip_prefix("//") {
        if let Some((scheme, _)) = base_url.split_once("://") {
            return format!("{scheme}://{rest}");
        }
        return href.to_owned();
    }
    if href.starts_with('/') {
        if let Some(origin) = url_origin(base_url) {
            return normalize_dot_segments(&format!("{origin}{href}"));
        }
    }

    let without_fragment = base_url.split_once('#').map_or(base_url, |(url, _)| url);
    let without_query = without_fragment
        .split_once('?')
        .map_or(without_fragment, |(url, _)| url);
    let base_dir = match without_query.rsplit_once('/') {
        Some((prefix, "")) => format!("{prefix}/"),
        Some((prefix, _)) => format!("{prefix}/"),
        None => String::new(),
    };
    normalize_dot_segments(&format!("{base_dir}{href}"))
}

fn normalize_dot_segments(url: &str) -> String {
    let (prefix, rest) = url_origin(url).map_or(("", url), |origin| (origin, &url[origin.len()..]));
    let suffix_start = rest.find(['?', '#']).unwrap_or(rest.len());
    let path = &rest[..suffix_start];
    let suffix = &rest[suffix_start..];
    let absolute = path.starts_with('/');
    let trailing_slash = path.ends_with('/');
    let mut segments = Vec::new();
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                segments.pop();
            }
            other => segments.push(other),
        }
    }

    let mut normalized = String::from(prefix);
    if absolute {
        normalized.push('/');
    }
    normalized.push_str(&segments.join("/"));
    if trailing_slash && !normalized.ends_with('/') {
        normalized.push('/');
    }
    normalized.push_str(suffix);
    normalized
}

fn strip_hash_fragment(url: &str) -> (String, BTreeMap<String, String>) {
    let Some((url, fragment)) = url.split_once('#') else {
        return (url.to_owned(), BTreeMap::new());
    };
    let mut hashes = BTreeMap::new();
    if let Some(digest) = fragment.strip_prefix("sha256=") {
        if !digest.is_empty() && digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            hashes.insert("sha256".to_owned(), digest.to_owned());
        }
    }
    (url.to_owned(), hashes)
}

fn url_origin(url: &str) -> Option<&str> {
    let scheme_end = url.find("://")?;
    let authority_start = scheme_end + "://".len();
    let authority_end = url[authority_start..]
        .find('/')
        .map_or(url.len(), |offset| authority_start + offset);
    Some(&url[..authority_end])
}

fn has_scheme(url: &str) -> bool {
    let Some(colon) = url.find(':') else {
        return false;
    };
    !url[..colon]
        .bytes()
        .any(|byte| matches!(byte, b'/' | b'?' | b'#'))
}

fn html_unescape(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(index) = rest.find('&') {
        output.push_str(&rest[..index]);
        rest = &rest[index..];
        if let Some(entity) = rest.strip_prefix("&amp;") {
            output.push('&');
            rest = entity;
        } else if let Some(entity) = rest.strip_prefix("&lt;") {
            output.push('<');
            rest = entity;
        } else if let Some(entity) = rest.strip_prefix("&gt;") {
            output.push('>');
            rest = entity;
        } else if let Some(entity) = rest.strip_prefix("&quot;") {
            output.push('"');
            rest = entity;
        } else if let Some(entity) = rest.strip_prefix("&#39;") {
            output.push('\'');
            rest = entity;
        } else {
            output.push('&');
            rest = &rest['&'.len_utf8()..];
        }
    }
    output.push_str(rest);
    output
}

fn find_ascii_case_insensitive(haystack: &str, needle: &str) -> Option<usize> {
    haystack
        .as_bytes()
        .windows(needle.len())
        .position(|window| window.eq_ignore_ascii_case(needle.as_bytes()))
}

fn is_attr_name_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b':')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resolve::source::PackageKind;

    #[test]
    fn parses_pep_503_anchor_metadata() {
        let body = r#"
            <html><body>
              <a href="../files/demo-1.0.tar.gz#sha256=abcdef" data-requires-python="&gt;=3.8" data-yanked="old" data-core-metadata="sha256=1234">demo-1.0.tar.gz</a>
            </body></html>
        "#;

        let page = parse_project_html("https://example.test/simple/Demo/", body)
            .expect("parse")
            .expect("page");

        assert_eq!(page.name, "demo");
        assert_eq!(page.files.len(), 1);
        let file = &page.files[0];
        assert_eq!(file.filename, "demo-1.0.tar.gz");
        assert_eq!(file.url, "https://example.test/simple/files/demo-1.0.tar.gz");
        assert_eq!(file.hashes.get("sha256").map(String::as_str), Some("abcdef"));
        assert_eq!(file.kind, PackageKind::Pure);
        assert_eq!(file.requires_python.as_ref().map(ToString::to_string), Some(">=3.8".to_owned()));
        assert_eq!(file.yanked.as_deref(), Some("old"));
        assert_eq!(
            file.dist_info_metadata
                .as_ref()
                .and_then(|metadata| metadata.hashes.get("sha256"))
                .map(String::as_str),
            Some("1234")
        );
    }
}

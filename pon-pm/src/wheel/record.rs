use crate::error::{Error, Result};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct RecordEntry {
    pub path: String,
    pub hash: Option<String>,
    pub size: Option<u64>,
}

pub(crate) fn parse_record(text: &str, label: &str) -> Result<Vec<RecordEntry>> {
    let mut entries = Vec::new();
    for (line_index, line) in text.lines().enumerate() {
        if line.is_empty() {
            continue;
        }
        let fields = parse_csv_line(line).map_err(|message| {
            Error::UnsupportedArtifact(format!(
                "invalid RECORD line {} in {label}: {message}",
                line_index + 1
            ))
        })?;
        if fields.len() != 3 {
            return Err(Error::UnsupportedArtifact(format!(
                "invalid RECORD line {} in {label}: expected 3 fields, found {}",
                line_index + 1,
                fields.len()
            )));
        }
        if fields[0].is_empty() {
            return Err(Error::UnsupportedArtifact(format!(
                "invalid RECORD line {} in {label}: empty path",
                line_index + 1
            )));
        }
        let size = if fields[2].is_empty() {
            None
        } else {
            Some(fields[2].parse::<u64>().map_err(|_| {
                Error::UnsupportedArtifact(format!(
                    "invalid RECORD line {} in {label}: invalid size `{}`",
                    line_index + 1,
                    fields[2]
                ))
            })?)
        };
        entries.push(RecordEntry {
            path: fields[0].clone(),
            hash: (!fields[1].is_empty()).then(|| fields[1].clone()),
            size,
        });
    }
    Ok(entries)
}

fn parse_csv_line(line: &str) -> std::result::Result<Vec<String>, String> {
    let mut fields = Vec::new();
    let mut field = String::new();
    let mut chars = line.chars().peekable();
    let mut quoted = false;

    while let Some(ch) = chars.next() {
        match ch {
            '"' if quoted && chars.peek() == Some(&'"') => {
                field.push('"');
                chars.next();
            }
            '"' => quoted = !quoted,
            ',' if !quoted => {
                fields.push(std::mem::take(&mut field));
            }
            _ => field.push(ch),
        }
    }

    if quoted {
        return Err("unterminated quoted field".to_owned());
    }
    fields.push(field);
    Ok(fields)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_record_rows_with_empty_hash_for_record_itself() {
        let entries = parse_record(
            "pkg/__init__.py,sha256=abc,12\npkg-1.0.dist-info/RECORD,,\n",
            "fixture",
        )
        .expect("record");

        assert_eq!(entries[0].path, "pkg/__init__.py");
        assert_eq!(entries[0].hash.as_deref(), Some("sha256=abc"));
        assert_eq!(entries[0].size, Some(12));
        assert_eq!(entries[1].path, "pkg-1.0.dist-info/RECORD");
        assert_eq!(entries[1].hash, None);
        assert_eq!(entries[1].size, None);
    }

    #[test]
    fn parses_quoted_commas() {
        let entries = parse_record("\"pkg/data,1.txt\",sha256=abc,4\n", "fixture").expect("record");
        assert_eq!(entries[0].path, "pkg/data,1.txt");
    }
}

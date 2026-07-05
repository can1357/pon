use crate::error::{Error, Result};

/// Normalizes a distribution name according to PEP 503.
///
/// Runs of `-`, `_`, and `.` collapse to one `-`, and ASCII letters are
/// lower-cased. Invalid names should be rejected with [`validate`] before use
/// when accepting user input.
#[must_use]
pub fn normalize(name: &str) -> String {
    let mut normalized = String::with_capacity(name.len());
    let mut last_was_separator = false;

    for byte in name.bytes() {
        match byte {
            b'A'..=b'Z' => {
                normalized.push((byte + 32) as char);
                last_was_separator = false;
            }
            b'a'..=b'z' | b'0'..=b'9' => {
                normalized.push(byte as char);
                last_was_separator = false;
            }
            b'-' | b'_' | b'.' => {
                if !last_was_separator {
                    normalized.push('-');
                    last_was_separator = true;
                }
            }
            _ => normalized.push(byte as char),
        }
    }

    normalized
}

/// Validates the project-name grammar used by Python packaging metadata.
pub fn validate(name: &str) -> Result<()> {
    let valid = !name.is_empty()
        && name
            .bytes()
            .all(|byte| matches!(byte, b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.'))
        && name
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_alphanumeric())
        && name
            .bytes()
            .last()
            .is_some_and(|byte| byte.is_ascii_alphanumeric());

    if valid {
        Ok(())
    } else {
        Err(Error::InvalidName(name.to_owned()))
    }
}

#[must_use]
pub fn normalized_eq(left: &str, right: &str) -> bool {
    normalize(left) == normalize(right)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pep_503_collapses_separator_runs() {
        assert_eq!(normalize("Friendly_Bard...Tools"), "friendly-bard-tools");
        assert_eq!(normalize("a--b__c..d"), "a-b-c-d");
    }

    #[test]
    fn validates_distribution_names() {
        assert!(validate("demo-pkg_1.0").is_ok());
        assert!(validate("-pon").is_err());
        assert!(validate("pon!").is_err());
        assert!(validate("").is_err());
    }

    #[test]
    fn normalized_comparison_ignores_allowed_separators_and_case() {
        assert!(normalized_eq("Demo_Pkg", "demo-pkg"));
    }
}

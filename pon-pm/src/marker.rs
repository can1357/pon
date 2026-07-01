use crate::error::{Error, Result};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarkerEnvironment {
    pub python_version: String,
    pub python_full_version: String,
    pub os_name: String,
    pub sys_platform: String,
    pub platform_machine: String,
    pub platform_system: String,
    pub extra: Option<String>,
}

impl MarkerEnvironment {
    #[must_use]
    pub fn current() -> Self {
        Self {
            python_version: "3.13".to_owned(),
            python_full_version: "3.13.0".to_owned(),
            os_name: std::env::consts::OS.to_owned(),
            sys_platform: std::env::consts::OS.to_owned(),
            platform_machine: std::env::consts::ARCH.to_owned(),
            platform_system: std::env::consts::OS.to_owned(),
            extra: None,
        }
    }

    fn value(&self, key: &str) -> Option<&str> {
        match key {
            "python_version" => Some(&self.python_version),
            "python_full_version" => Some(&self.python_full_version),
            "os_name" => Some(&self.os_name),
            "sys_platform" => Some(&self.sys_platform),
            "platform_machine" => Some(&self.platform_machine),
            "platform_system" => Some(&self.platform_system),
            "extra" => self.extra.as_deref(),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MarkerExpression {
    raw: String,
}

impl MarkerExpression {
    pub fn parse(raw: impl AsRef<str>) -> Result<Self> {
        let raw = raw.as_ref().trim();
        if raw.is_empty() {
            return Ok(Self { raw: String::new() });
        }
        if !raw.contains('"') && !raw.contains('\'') {
            return Err(Error::InvalidMarker(raw.to_owned()));
        }
        Ok(Self { raw: raw.to_owned() })
    }

    #[must_use]
    pub fn raw(&self) -> &str {
        &self.raw
    }

    pub fn evaluate(&self, env: &MarkerEnvironment) -> Result<bool> {
        if self.raw.is_empty() {
            return Ok(true);
        }

        self.raw
            .split(" or ")
            .map(|term| {
                term.split(" and ")
                    .map(|factor| evaluate_factor(factor.trim(), env))
                    .try_fold(true, |acc, value| value.map(|value| acc && value))
            })
            .try_fold(false, |acc, value| value.map(|value| acc || value))
    }
}

fn evaluate_factor(factor: &str, env: &MarkerEnvironment) -> Result<bool> {
    for op in ["==", "!=", ">=", "<=", ">", "<"] {
        if let Some((left, right)) = factor.split_once(op) {
            let left = left.trim();
            let right = unquote(right.trim()).ok_or_else(|| Error::InvalidMarker(factor.to_owned()))?;
            let value = env.value(left).ok_or_else(|| Error::InvalidMarker(factor.to_owned()))?;
            return Ok(compare_values(value, op, &right));
        }
    }
    Err(Error::InvalidMarker(factor.to_owned()))
}

fn unquote(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    if bytes.len() >= 2
        && ((bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"')
            || (bytes[0] == b'\'' && bytes[bytes.len() - 1] == b'\''))
    {
        Some(value[1..value.len() - 1].to_owned())
    } else {
        None
    }
}

fn compare_values(left: &str, op: &str, right: &str) -> bool {
    let ordering = version_key(left).cmp(&version_key(right));
    match op {
        "==" => left == right,
        "!=" => left != right,
        ">=" => ordering.is_ge(),
        "<=" => ordering.is_le(),
        ">" => ordering.is_gt(),
        "<" => ordering.is_lt(),
        _ => false,
    }
}

fn version_key(value: &str) -> Vec<u32> {
    value
        .split('.')
        .map(|part| part.parse::<u32>().unwrap_or(0))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env() -> MarkerEnvironment {
        MarkerEnvironment {
            python_version: "3.13".to_owned(),
            python_full_version: "3.13.1".to_owned(),
            os_name: "posix".to_owned(),
            sys_platform: "darwin".to_owned(),
            platform_machine: "arm64".to_owned(),
            platform_system: "Darwin".to_owned(),
            extra: Some("test".to_owned()),
        }
    }

    #[test]
    fn evaluates_supported_marker_boolean_forms() {
        let marker = MarkerExpression::parse(
            "python_version >= '3.12' and sys_platform == 'darwin' or extra == 'docs'",
        )
        .expect("marker");
        assert!(marker.evaluate(&env()).expect("eval"));
    }

    #[test]
    fn rejects_unknown_marker_variables() {
        let marker = MarkerExpression::parse("implementation_name == 'cpython'").expect("parse");
        assert!(marker.evaluate(&env()).is_err());
    }
}

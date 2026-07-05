use std::fmt::Write as _;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Status {
	Pass,
	Fail,
	SemanticsDivergent,
	Excluded,
	Unsupported,
}

impl Status {
	pub fn as_str(self) -> &'static str {
		match self {
			Self::Pass => "pass",
			Self::Fail => "fail",
			Self::SemanticsDivergent => "semantics-divergent",
			Self::Excluded => "excluded",
			Self::Unsupported => "unsupported",
		}
	}
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Record {
	pub module: String,
	pub status: Status,
	pub detail: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Scoreboard {
	pub suite:       String,
	pub cpython_tag: Option<String>,
	pub records:     Vec<Record>,
}

impl Scoreboard {
	pub fn new(suite: impl Into<String>, cpython_tag: Option<String>) -> Self {
		Self { suite: suite.into(), cpython_tag, records: Vec::new() }
	}

	pub fn push(&mut self, module: impl Into<String>, status: Status, detail: Option<String>) {
		self
			.records
			.push(Record { module: module.into(), status, detail });
	}

	pub fn has_status(&self, status: Status) -> bool {
		self.records.iter().any(|record| record.status == status)
	}

	pub fn pass_count(&self) -> usize {
		self.status_count(Status::Pass)
	}

	pub fn status_count(&self, status: Status) -> usize {
		self
			.records
			.iter()
			.filter(|record| record.status == status)
			.count()
	}

	pub fn status_for_module(&self, module: &str) -> Option<Status> {
		self
			.records
			.iter()
			.find(|record| record.module == module)
			.map(|record| record.status)
	}

	pub fn passing_modules(&self) -> Vec<String> {
		let mut modules = self
			.records
			.iter()
			.filter(|record| record.status == Status::Pass)
			.map(|record| record.module.clone())
			.collect::<Vec<_>>();
		modules.sort();
		modules.dedup();
		modules
	}

	pub fn to_json(&self) -> String {
		let mut json = String::new();
		json.push_str("{\n");
		write!(json, "  \"suite\": \"{}\"", escape_json(&self.suite))
			.expect("write to String cannot fail");
		if let Some(tag) = &self.cpython_tag {
			write!(json, ",\n  \"cpython_tag\": \"{}\"", escape_json(tag))
				.expect("write to String cannot fail");
		}
		json.push_str(",\n  \"summary\": {\n");
		write!(
			json,
			"    \"pass\": {},\n    \"fail\": {},\n    \"semantics-divergent\": {},\n    \
			 \"excluded\": {},\n    \"unsupported\": {}\n",
			self.status_count(Status::Pass),
			self.status_count(Status::Fail),
			self.status_count(Status::SemanticsDivergent),
			self.status_count(Status::Excluded),
			self.status_count(Status::Unsupported),
		)
		.expect("write to String cannot fail");
		json.push_str("  },\n  \"records\": [");
		if !self.records.is_empty() {
			json.push('\n');
		}
		for (index, record) in self.records.iter().enumerate() {
			let comma = if index + 1 == self.records.len() {
				""
			} else {
				","
			};
			write!(
				json,
				"    {{ \"module\": \"{}\", \"status\": \"{}\"",
				escape_json(&record.module),
				record.status.as_str(),
			)
			.expect("write to String cannot fail");
			if let Some(detail) = &record.detail {
				write!(json, ", \"detail\": \"{}\"", escape_json(detail))
					.expect("write to String cannot fail");
			}
			write!(json, " }}{comma}\n").expect("write to String cannot fail");
		}
		json.push_str("  ]\n}\n");
		json
	}
}

fn escape_json(value: &str) -> String {
	let mut escaped = String::new();
	for character in value.chars() {
		match character {
			'"' => escaped.push_str("\\\""),
			'\\' => escaped.push_str("\\\\"),
			'\n' => escaped.push_str("\\n"),
			'\r' => escaped.push_str("\\r"),
			'\t' => escaped.push_str("\\t"),
			character if character.is_control() => {
				write!(escaped, "\\u{:04x}", character as u32).expect("write to String cannot fail");
			},
			character => escaped.push(character),
		}
	}
	escaped
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn scoreboard_serializes_statuses_and_escapes_details() {
		let mut scoreboard = Scoreboard::new("cpython", Some("v3.14.0".to_owned()));
		scoreboard.push("Lib/test/test_pass.py", Status::Pass, None);
		scoreboard.push(
			"Lib/test/test_quote\".py",
			Status::Unsupported,
			Some("missing\nmodule".to_owned()),
		);
		scoreboard.push(
			"test.test_ctypes.test_numbers",
			Status::Excluded,
			Some("excluded by test_ctypes* (c-abi-boundary)".to_owned()),
		);

		let json = scoreboard.to_json();

		assert!(json.contains("\"suite\": \"cpython\""));
		assert!(json.contains("\"cpython_tag\": \"v3.14.0\""));
		assert!(json.contains("\"pass\": 1"));
		assert!(json.contains("\"excluded\": 1"));
		assert!(json.contains("\"unsupported\": 1"));
		assert!(json.contains("\"status\": \"excluded\""));
		assert!(json.contains("Lib/test/test_quote\\\".py"));
		assert!(json.contains("missing\\nmodule"));
	}
}

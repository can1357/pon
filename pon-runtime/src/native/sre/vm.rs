use std::{
	collections::{BTreeMap, HashSet},
	fmt,
	ops::Deref,
	sync::Arc,
};

pub const MAGIC: u32 = 20_230_612;
pub const CODESIZE: usize = 4;
pub const MAXREPEAT: u32 = u32::MAX;

const FAILURE: u32 = 0;
const SUCCESS: u32 = 1;
const ANY: u32 = 2;
const ANY_ALL: u32 = 3;
const ASSERT: u32 = 4;
const ASSERT_NOT: u32 = 5;
const AT: u32 = 6;
const BRANCH: u32 = 7;
const CATEGORY: u32 = 8;
const CHARSET: u32 = 9;
const BIGCHARSET: u32 = 10;
const GROUPREF: u32 = 11;
const GROUPREF_EXISTS: u32 = 12;
const IN: u32 = 13;
const INFO: u32 = 14;
const JUMP: u32 = 15;
const LITERAL: u32 = 16;
const MARK: u32 = 17;
const MAX_UNTIL: u32 = 18;
const MIN_UNTIL: u32 = 19;
const NOT_LITERAL: u32 = 20;
const NEGATE: u32 = 21;
const RANGE: u32 = 22;
const REPEAT: u32 = 23;
const REPEAT_ONE: u32 = 24;
const MIN_REPEAT_ONE: u32 = 26;
const ATOMIC_GROUP: u32 = 27;
const POSSESSIVE_REPEAT: u32 = 28;
const POSSESSIVE_REPEAT_ONE: u32 = 29;
const GROUPREF_IGNORE: u32 = 30;
const IN_IGNORE: u32 = 31;
const LITERAL_IGNORE: u32 = 32;
const NOT_LITERAL_IGNORE: u32 = 33;
const GROUPREF_LOC_IGNORE: u32 = 34;
const IN_LOC_IGNORE: u32 = 35;
const LITERAL_LOC_IGNORE: u32 = 36;
const NOT_LITERAL_LOC_IGNORE: u32 = 37;
const GROUPREF_UNI_IGNORE: u32 = 38;
const IN_UNI_IGNORE: u32 = 39;
const LITERAL_UNI_IGNORE: u32 = 40;
const NOT_LITERAL_UNI_IGNORE: u32 = 41;
const RANGE_UNI_IGNORE: u32 = 42;

const AT_BEGINNING: u32 = 0;
const AT_BEGINNING_LINE: u32 = 1;
const AT_BEGINNING_STRING: u32 = 2;
const AT_BOUNDARY: u32 = 3;
const AT_NON_BOUNDARY: u32 = 4;
const AT_END: u32 = 5;
const AT_END_LINE: u32 = 6;
const AT_END_STRING: u32 = 7;
const AT_LOC_BOUNDARY: u32 = 8;
const AT_LOC_NON_BOUNDARY: u32 = 9;
const AT_UNI_BOUNDARY: u32 = 10;
const AT_UNI_NON_BOUNDARY: u32 = 11;

const CATEGORY_DIGIT: u32 = 0;
const CATEGORY_NOT_DIGIT: u32 = 1;
const CATEGORY_SPACE: u32 = 2;
const CATEGORY_NOT_SPACE: u32 = 3;
const CATEGORY_WORD: u32 = 4;
const CATEGORY_NOT_WORD: u32 = 5;
const CATEGORY_LINEBREAK: u32 = 6;
const CATEGORY_NOT_LINEBREAK: u32 = 7;
const CATEGORY_LOC_WORD: u32 = 8;
const CATEGORY_LOC_NOT_WORD: u32 = 9;
const CATEGORY_UNI_DIGIT: u32 = 10;
const CATEGORY_UNI_NOT_DIGIT: u32 = 11;
const CATEGORY_UNI_SPACE: u32 = 12;
const CATEGORY_UNI_NOT_SPACE: u32 = 13;
const CATEGORY_UNI_WORD: u32 = 14;
const CATEGORY_UNI_NOT_WORD: u32 = 15;
const CATEGORY_UNI_LINEBREAK: u32 = 16;
const CATEGORY_UNI_NOT_LINEBREAK: u32 = 17;

const STEP_LIMIT: usize = 4_000_000;
const RESULT_LIMIT: usize = 200_000;

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Error {
	MagicMismatch { expected: u32, got: u32 },
	Truncated { pc: usize, needed: usize },
	InvalidOpcode { pc: usize, opcode: u32 },
	InvalidSkip { pc: usize, skip: u32 },
	UnsupportedOpcode { pc: usize, opcode: u32 },
	ExecutionLimit,
}

impl fmt::Display for Error {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Self::MagicMismatch { expected, got } => {
				write!(f, "_sre MAGIC mismatch: expected {expected}, got {got}")
			},
			Self::Truncated { pc, needed } => {
				write!(f, "truncated SRE code at {pc}, need {needed} words")
			},
			Self::InvalidOpcode { pc, opcode } => write!(f, "invalid SRE opcode {opcode} at {pc}"),
			Self::InvalidSkip { pc, skip } => write!(f, "invalid SRE skip {skip} at {pc}"),
			Self::UnsupportedOpcode { pc, opcode } => {
				write!(f, "unsupported SRE opcode {opcode} at {pc}")
			},
			Self::ExecutionLimit => write!(f, "SRE execution limit exceeded"),
		}
	}
}

impl std::error::Error for Error {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CaseMode {
	Exact,
	Ascii,
	Locale,
	Unicode,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PatternText {
	Str(String),
	Bytes(Vec<u8>),
	Unknown,
}

#[derive(Clone, Debug)]
pub struct Pattern {
	pattern:    PatternText,
	flags:      u32,
	code:       Vec<u32>,
	groups:     usize,
	groupindex: BTreeMap<String, usize>,
	indexgroup: Vec<Option<String>>,
	nodes:      Vec<Node>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum MatchedValue {
	Str(String),
	Bytes(Vec<u8>),
}

#[derive(Clone, Debug)]
pub struct Match {
	pattern:    PatternText,
	subject:    SubjectData,
	spans:      Vec<Option<(usize, usize)>>,
	lastindex:  Option<usize>,
	lastgroup:  Option<String>,
	groupindex: BTreeMap<String, usize>,
}

#[derive(Clone, Debug)]
struct SubjectData {
	shared: Arc<SubjectBacking>,
}

#[derive(Debug)]
struct SubjectBacking {
	kind:    SubjectKind,
	text:    String,
	bytes:   Vec<u8>,
	units:   Vec<u32>,
	offsets: Vec<usize>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SubjectKind {
	Str,
	Bytes,
}

#[derive(Clone, Debug)]
struct Program {
	nodes: Vec<Node>,
}

#[derive(Clone, Debug)]
enum Node {
	Literal { value: u32, case: CaseMode },
	NotLiteral { value: u32, case: CaseMode },
	Any { all: bool },
	At(u32),
	In { set: Charset, case: CaseMode },
	Mark(usize),
	GroupRef { group: usize, case: CaseMode },
	GroupRefExists { group: usize, yes: Vec<Node>, no: Vec<Node> },
	Branch(Vec<Vec<Node>>),
	Repeat { body: Vec<Node>, min: usize, max: Option<usize>, kind: RepeatKind },
	Assert { positive: bool, width: usize, body: Vec<Node> },
	Atomic(Vec<Node>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RepeatKind {
	Greedy,
	Lazy,
	Possessive,
}

#[derive(Clone, Debug)]
struct Charset {
	items:   Vec<SetItem>,
	negated: bool,
}

#[derive(Clone, Debug)]
enum SetItem {
	Literal(u32),
	Range(u32, u32),
	RangeUnicodeIgnore(u32, u32),
	Bitmap(Vec<u32>),
	BigCharset(Vec<u32>),
	Category(u32),
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct MatchState {
	pos:       usize,
	marks:     Vec<Option<usize>>,
	lastindex: Option<usize>,
}

#[derive(Clone, Copy)]
struct Segment<'a> {
	nodes: &'a [Node],
	index: usize,
}

#[derive(Clone)]
struct Thread<'a> {
	segments: Vec<Segment<'a>>,
	state:    MatchState,
}

pub fn getcodesize() -> usize {
	CODESIZE
}

pub fn compile(
	pattern: PatternText,
	flags: u32,
	code: Vec<u32>,
	groups: usize,
	groupindex: BTreeMap<String, usize>,
	indexgroup: Vec<Option<String>>,
) -> Result<Pattern, Error> {
	compile_checked(MAGIC, pattern, flags, code, groups, groupindex, indexgroup)
}

pub fn compile_checked(
	magic: u32,
	pattern: PatternText,
	flags: u32,
	code: Vec<u32>,
	groups: usize,
	groupindex: BTreeMap<String, usize>,
	indexgroup: Vec<Option<String>>,
) -> Result<Pattern, Error> {
	if magic != MAGIC {
		return Err(Error::MagicMismatch { expected: MAGIC, got: magic });
	}
	let program = Program::parse(&code)?;
	Ok(Pattern { pattern, flags, code, groups, groupindex, indexgroup, nodes: program.nodes })
}

impl Pattern {
	pub fn pattern(&self) -> &PatternText {
		&self.pattern
	}

	pub fn flags(&self) -> u32 {
		self.flags
	}

	pub fn code(&self) -> &[u32] {
		&self.code
	}

	pub fn groups(&self) -> usize {
		self.groups
	}

	pub fn groupindex(&self) -> &BTreeMap<String, usize> {
		&self.groupindex
	}

	pub fn indexgroup(&self) -> &[Option<String>] {
		&self.indexgroup
	}

	pub fn match_str(&self, subject: &str) -> Result<Option<Match>, Error> {
		self.match_data(&SubjectData::from_str(subject), 0, None)
	}

	pub fn match_bytes(&self, subject: &[u8]) -> Result<Option<Match>, Error> {
		self.match_data(&SubjectData::from_bytes(subject), 0, None)
	}

	pub fn fullmatch_str(&self, subject: &str) -> Result<Option<Match>, Error> {
		let data = SubjectData::from_str(subject);
		let end = data.len();
		self.match_data(&data, 0, Some(end))
	}

	pub fn fullmatch_bytes(&self, subject: &[u8]) -> Result<Option<Match>, Error> {
		let data = SubjectData::from_bytes(subject);
		let end = data.len();
		self.match_data(&data, 0, Some(end))
	}

	pub fn search_str(&self, subject: &str) -> Result<Option<Match>, Error> {
		self.search_data(&SubjectData::from_str(subject))
	}

	pub fn search_bytes(&self, subject: &[u8]) -> Result<Option<Match>, Error> {
		self.search_data(&SubjectData::from_bytes(subject))
	}

	pub fn finditer_str(&self, subject: &str) -> Result<Vec<Match>, Error> {
		self.finditer_data(&SubjectData::from_str(subject))
	}

	pub fn finditer_bytes(&self, subject: &[u8]) -> Result<Vec<Match>, Error> {
		self.finditer_data(&SubjectData::from_bytes(subject))
	}

	pub fn match_str_at(&self, subject: &str, pos: usize) -> Result<Option<Match>, Error> {
		self.match_data(&SubjectData::from_str(subject), pos, None)
	}

	pub fn match_bytes_at(&self, subject: &[u8], pos: usize) -> Result<Option<Match>, Error> {
		self.match_data(&SubjectData::from_bytes(subject), pos, None)
	}

	pub fn fullmatch_str_at(&self, subject: &str, pos: usize) -> Result<Option<Match>, Error> {
		let data = SubjectData::from_str(subject);
		let end = data.len();
		self.match_data(&data, pos, Some(end))
	}

	pub fn fullmatch_bytes_at(&self, subject: &[u8], pos: usize) -> Result<Option<Match>, Error> {
		let data = SubjectData::from_bytes(subject);
		let end = data.len();
		self.match_data(&data, pos, Some(end))
	}

	pub fn search_str_at(&self, subject: &str, pos: usize) -> Result<Option<Match>, Error> {
		self.search_data_from(&SubjectData::from_str(subject), pos)
	}

	pub fn search_bytes_at(&self, subject: &[u8], pos: usize) -> Result<Option<Match>, Error> {
		self.search_data_from(&SubjectData::from_bytes(subject), pos)
	}

	pub fn findall_str(&self, subject: &str) -> Result<Vec<Vec<Option<MatchedValue>>>, Error> {
		Ok(self
			.finditer_str(subject)?
			.iter()
			.map(Match::findall_groups)
			.collect())
	}

	pub fn findall_bytes(&self, subject: &[u8]) -> Result<Vec<Vec<Option<MatchedValue>>>, Error> {
		Ok(self
			.finditer_bytes(subject)?
			.iter()
			.map(Match::findall_groups)
			.collect())
	}

	pub fn split_str(&self, subject: &str) -> Result<Vec<MatchedValue>, Error> {
		self.split_data(&SubjectData::from_str(subject))
	}

	pub fn split_bytes(&self, subject: &[u8]) -> Result<Vec<MatchedValue>, Error> {
		self.split_data(&SubjectData::from_bytes(subject))
	}

	pub fn sub_str(&self, repl: &str, subject: &str) -> Result<String, Error> {
		let value =
			self.sub_data(MatchedValue::Str(repl.to_owned()), &SubjectData::from_str(subject))?;
		match value {
			MatchedValue::Str(text) => Ok(text),
			MatchedValue::Bytes(bytes) => Ok(String::from_utf8_lossy(&bytes).into_owned()),
		}
	}

	pub fn sub_bytes(&self, repl: &[u8], subject: &[u8]) -> Result<Vec<u8>, Error> {
		let value =
			self.sub_data(MatchedValue::Bytes(repl.to_vec()), &SubjectData::from_bytes(subject))?;
		match value {
			MatchedValue::Str(text) => Ok(text.into_bytes()),
			MatchedValue::Bytes(bytes) => Ok(bytes),
		}
	}

	fn search_data(&self, data: &SubjectData) -> Result<Option<Match>, Error> {
		self.search_data_from(data, 0)
	}

	/// Search for the leftmost match at or after `start` (CPython
	/// `Pattern.search`'s `pos`), scanning successive start offsets.
	fn search_data_from(&self, data: &SubjectData, start: usize) -> Result<Option<Match>, Error> {
		for pos in start..=data.len() {
			if let Some(matched) = self.match_data(data, pos, None)? {
				return Ok(Some(matched));
			}
		}
		Ok(None)
	}

	fn match_data(
		&self,
		data: &SubjectData,
		start: usize,
		require_end: Option<usize>,
	) -> Result<Option<Match>, Error> {
		self.match_data_filtered(data, start, require_end, false)
	}

	fn match_data_filtered(
		&self,
		data: &SubjectData,
		start: usize,
		require_end: Option<usize>,
		skip_empty_at_start: bool,
	) -> Result<Option<Match>, Error> {
		let start_state =
			MatchState { pos: start, marks: vec![None; self.groups * 2], lastindex: None };
		let states =
			execute(&self.nodes, data, start_state, require_end.is_some() || skip_empty_at_start)?;
		for state in states {
			if skip_empty_at_start && state.pos == start {
				continue;
			}
			if require_end.is_none_or(|end| state.pos == end) {
				return Ok(Some(self.build_match(SubjectData::clone(data), start, state)));
			}
		}
		Ok(None)
	}

	fn finditer_data(&self, data: &SubjectData) -> Result<Vec<Match>, Error> {
		let mut out = Vec::new();
		let mut start = 0;
		let mut skip_empty_at = None;
		while start <= data.len() {
			let skip_empty = skip_empty_at == Some(start);
			let Some(matched) = self.match_data_filtered(data, start, None, skip_empty)? else {
				skip_empty_at = None;
				start += 1;
				continue;
			};
			let end = matched.end(0).unwrap_or(start);
			if end == start {
				skip_empty_at = Some(start);
			} else {
				skip_empty_at = None;
				start = end;
			}
			out.push(matched);
			if end == start {
				continue;
			}
			if start > data.len() {
				break;
			}
		}
		Ok(out)
	}

	fn split_data(&self, data: &SubjectData) -> Result<Vec<MatchedValue>, Error> {
		let mut out = Vec::new();
		let mut last = 0;
		for matched in self.finditer_data(data)? {
			let (start, end) = matched.span(0).flatten().unwrap_or((last, last));
			out.push(data.slice_value(last, start));
			for group in 1..=self.groups {
				if let Some((group_start, group_end)) = matched.span(group).flatten() {
					out.push(data.slice_value(group_start, group_end));
				}
			}
			last = end;
		}
		out.push(data.slice_value(last, data.len()));
		Ok(out)
	}

	fn sub_data(&self, repl: MatchedValue, data: &SubjectData) -> Result<MatchedValue, Error> {
		match data.kind {
			SubjectKind::Str => {
				let mut out = String::new();
				let replacement = match &repl {
					MatchedValue::Str(text) => text.clone(),
					MatchedValue::Bytes(bytes) => String::from_utf8_lossy(bytes).into_owned(),
				};
				let mut last = 0;
				for matched in self.finditer_data(data)? {
					let (start, end) = matched.span(0).flatten().unwrap_or((last, last));
					if let MatchedValue::Str(prefix) = data.slice_value(last, start) {
						out.push_str(&prefix);
					}
					out.push_str(&replacement);
					last = end;
				}
				if let MatchedValue::Str(suffix) = data.slice_value(last, data.len()) {
					out.push_str(&suffix);
				}
				Ok(MatchedValue::Str(out))
			},
			SubjectKind::Bytes => {
				let mut out = Vec::new();
				let replacement = match &repl {
					MatchedValue::Str(text) => text.as_bytes().to_vec(),
					MatchedValue::Bytes(bytes) => bytes.clone(),
				};
				let mut last = 0;
				for matched in self.finditer_data(data)? {
					let (start, end) = matched.span(0).flatten().unwrap_or((last, last));
					if let MatchedValue::Bytes(prefix) = data.slice_value(last, start) {
						out.extend(prefix);
					}
					out.extend(&replacement);
					last = end;
				}
				if let MatchedValue::Bytes(suffix) = data.slice_value(last, data.len()) {
					out.extend(suffix);
				}
				Ok(MatchedValue::Bytes(out))
			},
		}
	}

	fn build_match(&self, subject: SubjectData, start: usize, state: MatchState) -> Match {
		let mut spans = Vec::with_capacity(self.groups + 1);
		spans.push(Some((start, state.pos)));
		for group in 0..self.groups {
			let mark = group * 2;
			spans.push(match (state.marks.get(mark), state.marks.get(mark + 1)) {
				(Some(Some(group_start)), Some(Some(group_end))) => Some((*group_start, *group_end)),
				_ => None,
			});
		}
		let lastindex = state
			.lastindex
			.filter(|index| spans.get(*index).is_some_and(Option::is_some));
		let lastgroup = lastindex.and_then(|index| self.indexgroup.get(index).cloned().flatten());
		Match {
			pattern: self.pattern.clone(),
			subject,
			spans,
			lastindex,
			lastgroup,
			groupindex: self.groupindex.clone(),
		}
	}
}

impl Match {
	pub fn pattern(&self) -> &PatternText {
		&self.pattern
	}

	pub fn group(&self, index: usize) -> Option<MatchedValue> {
		let (start, end) = self.span(index)??;
		Some(self.subject.slice_value(start, end))
	}

	pub fn group_name(&self, name: &str) -> Option<MatchedValue> {
		self
			.groupindex
			.get(name)
			.and_then(|index| self.group(*index))
	}

	pub fn groups(&self) -> Vec<Option<MatchedValue>> {
		(1..self.spans.len())
			.map(|index| self.group(index))
			.collect()
	}

	pub fn groupdict(&self) -> BTreeMap<String, Option<MatchedValue>> {
		self
			.groupindex
			.iter()
			.map(|(name, index)| (name.clone(), self.group(*index)))
			.collect()
	}

	pub fn start(&self, index: usize) -> Option<usize> {
		self.span(index).flatten().map(|(start, _)| start)
	}

	pub fn end(&self, index: usize) -> Option<usize> {
		self.span(index).flatten().map(|(_, end)| end)
	}

	pub fn span(&self, index: usize) -> Option<Option<(usize, usize)>> {
		self.spans.get(index).copied()
	}

	pub fn lastindex(&self) -> Option<usize> {
		self.lastindex
	}

	pub fn lastgroup(&self) -> Option<&str> {
		self.lastgroup.as_deref()
	}

	pub fn groupindex(&self) -> &BTreeMap<String, usize> {
		&self.groupindex
	}

	fn findall_groups(&self) -> Vec<Option<MatchedValue>> {
		if self.spans.len() == 1 {
			vec![self.group(0)]
		} else if self.spans.len() == 2 {
			vec![self.group(1)]
		} else {
			self.groups()
		}
	}
}

impl Deref for SubjectData {
	type Target = SubjectBacking;

	fn deref(&self) -> &Self::Target {
		self.shared.as_ref()
	}
}

impl SubjectData {
	fn from_str(subject: &str) -> Self {
		let mut units = Vec::new();
		let mut offsets = Vec::new();
		for (offset, ch) in subject.char_indices() {
			offsets.push(offset);
			units.push(ch as u32);
		}
		offsets.push(subject.len());
		Self {
			shared: Arc::new(SubjectBacking {
				kind: SubjectKind::Str,
				text: subject.to_owned(),
				bytes: Vec::new(),
				units,
				offsets,
			}),
		}
	}

	fn from_bytes(subject: &[u8]) -> Self {
		let units = subject
			.iter()
			.map(|byte| u32::from(*byte))
			.collect::<Vec<_>>();
		let offsets = (0..=subject.len()).collect::<Vec<_>>();
		Self {
			shared: Arc::new(SubjectBacking {
				kind: SubjectKind::Bytes,
				text: String::new(),
				bytes: subject.to_vec(),
				units,
				offsets,
			}),
		}
	}

	fn len(&self) -> usize {
		self.units.len()
	}

	fn unit(&self, index: usize) -> Option<u32> {
		self.units.get(index).copied()
	}

	fn slice_value(&self, start: usize, end: usize) -> MatchedValue {
		match self.kind {
			SubjectKind::Str => {
				let start_offset = self.offsets[start];
				let end_offset = self.offsets[end];
				MatchedValue::Str(self.text[start_offset..end_offset].to_owned())
			},
			SubjectKind::Bytes => MatchedValue::Bytes(self.bytes[start..end].to_vec()),
		}
	}
}

impl Program {
	fn parse(code: &[u32]) -> Result<Self, Error> {
		if code.is_empty() {
			return Err(Error::Truncated { pc: 0, needed: 1 });
		}
		let mut start = 0;
		if code[0] == INFO {
			need(code, 0, 2)?;
			let skip = as_usize(code[1]);
			let next = 1usize
				.checked_add(skip)
				.ok_or(Error::InvalidSkip { pc: 1, skip: code[1] })?;
			if next > code.len() {
				return Err(Error::InvalidSkip { pc: 1, skip: code[1] });
			}
			start = next;
		}
		let (nodes, _) = parse_sequence(code, start, code.len())?;
		Ok(Self { nodes })
	}
}

fn parse_sequence(code: &[u32], mut pc: usize, end: usize) -> Result<(Vec<Node>, usize), Error> {
	let mut nodes = Vec::new();
	while pc < end {
		let op = code[pc];
		match op {
			SUCCESS | FAILURE | JUMP | MAX_UNTIL | MIN_UNTIL => break,
			ANY => {
				nodes.push(Node::Any { all: false });
				pc += 1;
			},
			ANY_ALL => {
				nodes.push(Node::Any { all: true });
				pc += 1;
			},
			LITERAL | LITERAL_IGNORE | LITERAL_LOC_IGNORE | LITERAL_UNI_IGNORE => {
				need(code, pc, 2)?;
				nodes.push(Node::Literal { value: code[pc + 1], case: literal_case(op) });
				pc += 2;
			},
			NOT_LITERAL | NOT_LITERAL_IGNORE | NOT_LITERAL_LOC_IGNORE | NOT_LITERAL_UNI_IGNORE => {
				need(code, pc, 2)?;
				nodes.push(Node::NotLiteral { value: code[pc + 1], case: literal_case(op) });
				pc += 2;
			},
			AT => {
				need(code, pc, 2)?;
				nodes.push(Node::At(code[pc + 1]));
				pc += 2;
			},
			CATEGORY => {
				need(code, pc, 2)?;
				nodes.push(Node::In {
					set:  Charset { items: vec![SetItem::Category(code[pc + 1])], negated: false },
					case: CaseMode::Exact,
				});
				pc += 2;
			},
			IN | IN_IGNORE | IN_LOC_IGNORE | IN_UNI_IGNORE => {
				need(code, pc, 2)?;
				let next = checked_skip_end(pc, code[pc + 1])?;
				if next > code.len() {
					return Err(Error::InvalidSkip { pc: pc + 1, skip: code[pc + 1] });
				}
				let set = parse_charset(code, pc + 2, next)?;
				nodes.push(Node::In { set, case: in_case(op) });
				pc = next;
			},
			MARK => {
				need(code, pc, 2)?;
				nodes.push(Node::Mark(as_usize(code[pc + 1])));
				pc += 2;
			},
			GROUPREF | GROUPREF_IGNORE | GROUPREF_LOC_IGNORE | GROUPREF_UNI_IGNORE => {
				need(code, pc, 2)?;
				nodes.push(Node::GroupRef { group: as_usize(code[pc + 1]), case: groupref_case(op) });
				pc += 2;
			},
			ASSERT | ASSERT_NOT => {
				need(code, pc, 3)?;
				let next = checked_skip_end(pc, code[pc + 1])?;
				if next > code.len() || next == 0 {
					return Err(Error::InvalidSkip { pc: pc + 1, skip: code[pc + 1] });
				}
				let (body, _) = parse_sequence(code, pc + 3, next - 1)?;
				nodes.push(Node::Assert {
					positive: op == ASSERT,
					width: as_usize(code[pc + 2]),
					body,
				});
				pc = next;
			},
			BRANCH => {
				let (branches, next) = parse_branch(code, pc)?;
				nodes.push(Node::Branch(branches));
				pc = next;
			},
			REPEAT_ONE | MIN_REPEAT_ONE | POSSESSIVE_REPEAT_ONE => {
				need(code, pc, 4)?;
				let next = checked_skip_end(pc, code[pc + 1])?;
				if next > code.len() || next == 0 {
					return Err(Error::InvalidSkip { pc: pc + 1, skip: code[pc + 1] });
				}
				let (body, _) = parse_sequence(code, pc + 4, next - 1)?;
				nodes.push(Node::Repeat {
					body,
					min: as_usize(code[pc + 2]),
					max: max_repeat(code[pc + 3]),
					kind: match op {
						MIN_REPEAT_ONE => RepeatKind::Lazy,
						POSSESSIVE_REPEAT_ONE => RepeatKind::Possessive,
						_ => RepeatKind::Greedy,
					},
				});
				pc = next;
			},
			REPEAT => {
				need(code, pc, 4)?;
				let until = checked_skip_end(pc, code[pc + 1])?;
				if until >= code.len() {
					return Err(Error::InvalidSkip { pc: pc + 1, skip: code[pc + 1] });
				}
				let kind = match code[until] {
					MAX_UNTIL => RepeatKind::Greedy,
					MIN_UNTIL => RepeatKind::Lazy,
					other => return Err(Error::InvalidOpcode { pc: until, opcode: other }),
				};
				let (body, _) = parse_sequence(code, pc + 4, until)?;
				nodes.push(Node::Repeat {
					body,
					min: as_usize(code[pc + 2]),
					max: max_repeat(code[pc + 3]),
					kind,
				});
				pc = until + 1;
			},
			POSSESSIVE_REPEAT => {
				need(code, pc, 4)?;
				let next = checked_skip_end(pc, code[pc + 1])?;
				if next >= code.len() {
					return Err(Error::InvalidSkip { pc: pc + 1, skip: code[pc + 1] });
				}
				if code[next] != SUCCESS {
					return Err(Error::InvalidOpcode { pc: next, opcode: code[next] });
				}
				let (body, _) = parse_sequence(code, pc + 4, next)?;
				nodes.push(Node::Repeat {
					body,
					min: as_usize(code[pc + 2]),
					max: max_repeat(code[pc + 3]),
					kind: RepeatKind::Possessive,
				});
				pc = next + 1;
			},
			ATOMIC_GROUP => {
				need(code, pc, 2)?;
				let next = checked_skip_end(pc, code[pc + 1])?;
				if next > code.len() || next == 0 {
					return Err(Error::InvalidSkip { pc: pc + 1, skip: code[pc + 1] });
				}
				let (body, _) = parse_sequence(code, pc + 2, next - 1)?;
				nodes.push(Node::Atomic(body));
				pc = next;
			},
			GROUPREF_EXISTS => {
				let (node, next) = parse_groupref_exists(code, pc)?;
				nodes.push(node);
				pc = next;
			},
			INFO => return Err(Error::UnsupportedOpcode { pc, opcode: op }),
			opcode => return Err(Error::InvalidOpcode { pc, opcode }),
		}
	}
	Ok((nodes, pc))
}

fn parse_branch(code: &[u32], pc: usize) -> Result<(Vec<Vec<Node>>, usize), Error> {
	let mut cursor = pc + 1;
	let mut branches = Vec::new();
	loop {
		need(code, cursor, 1)?;
		if code[cursor] == FAILURE {
			return Ok((branches, cursor + 1));
		}
		let next = cursor
			.checked_add(as_usize(code[cursor]))
			.ok_or(Error::InvalidSkip { pc: cursor, skip: code[cursor] })?;
		if next > code.len() || next < cursor + 1 {
			return Err(Error::InvalidSkip { pc: cursor, skip: code[cursor] });
		}
		let branch_end = if next >= 2 && code[next - 2] == JUMP {
			next - 2
		} else {
			next
		};
		let (branch, _) = parse_sequence(code, cursor + 1, branch_end)?;
		branches.push(branch);
		cursor = next;
	}
}

fn parse_groupref_exists(code: &[u32], pc: usize) -> Result<(Node, usize), Error> {
	need(code, pc, 3)?;
	let group = as_usize(code[pc + 1]);
	let skip_pc = pc + 2;
	let no_start = skip_pc
		.checked_add(as_usize(code[skip_pc]))
		.and_then(|value| value.checked_sub(1))
		.ok_or(Error::InvalidSkip { pc: skip_pc, skip: code[skip_pc] })?;
	if no_start > code.len() {
		return Err(Error::InvalidSkip { pc: skip_pc, skip: code[skip_pc] });
	}
	let (yes_end, no, next) = if no_start >= 2 && code[no_start - 2] == JUMP {
		let jump_pc = no_start - 2;
		let jump_arg_pc = no_start - 1;
		let no_end = jump_arg_pc
			.checked_add(as_usize(code[jump_arg_pc]))
			.ok_or(Error::InvalidSkip { pc: jump_arg_pc, skip: code[jump_arg_pc] })?;
		if no_end > code.len() {
			return Err(Error::InvalidSkip { pc: jump_arg_pc, skip: code[jump_arg_pc] });
		}
		let (no_nodes, _) = parse_sequence(code, no_start, no_end)?;
		(jump_pc, no_nodes, no_end)
	} else {
		(no_start, Vec::new(), no_start)
	};
	let (yes, _) = parse_sequence(code, pc + 3, yes_end)?;
	Ok((Node::GroupRefExists { group, yes, no }, next))
}

fn parse_charset(code: &[u32], mut pc: usize, end: usize) -> Result<Charset, Error> {
	let mut items = Vec::new();
	let mut negated = false;
	while pc < end {
		match code[pc] {
			FAILURE => break,
			NEGATE => {
				negated = !negated;
				pc += 1;
			},
			LITERAL => {
				need(code, pc, 2)?;
				items.push(SetItem::Literal(code[pc + 1]));
				pc += 2;
			},
			RANGE => {
				need(code, pc, 3)?;
				items.push(SetItem::Range(code[pc + 1], code[pc + 2]));
				pc += 3;
			},
			RANGE_UNI_IGNORE => {
				need(code, pc, 3)?;
				items.push(SetItem::RangeUnicodeIgnore(code[pc + 1], code[pc + 2]));
				pc += 3;
			},
			CHARSET => {
				need(code, pc, 1 + CODESIZE * 2)?;
				let start = pc + 1;
				let bitmap_len = CODESIZE * 2;
				items.push(SetItem::Bitmap(code[start..start + bitmap_len].to_vec()));
				pc = start + bitmap_len;
			},
			BIGCHARSET => {
				need(code, pc, 2)?;
				let blocks = as_usize(code[pc + 1]);
				let len = 2 + 64 + blocks * 8;
				need(code, pc, len)?;
				items.push(SetItem::BigCharset(code[pc + 1..pc + len].to_vec()));
				pc += len;
			},
			CATEGORY => {
				need(code, pc, 2)?;
				items.push(SetItem::Category(code[pc + 1]));
				pc += 2;
			},
			opcode => return Err(Error::InvalidOpcode { pc, opcode }),
		}
	}
	Ok(Charset { items, negated })
}

fn execute(
	nodes: &[Node],
	subject: &SubjectData,
	start: MatchState,
	collect_all: bool,
) -> Result<Vec<MatchState>, Error> {
	let mut out = Vec::new();
	let mut stack = vec![Thread { segments: vec![Segment { nodes, index: 0 }], state: start }];
	let mut steps = 0usize;
	while let Some(mut thread) = stack.pop() {
		steps += 1;
		if steps > STEP_LIMIT {
			return Err(Error::ExecutionLimit);
		}
		while let Some(segment) = thread.segments.last() {
			if segment.index < segment.nodes.len() {
				break;
			}
			thread.segments.pop();
		}
		let Some(segment) = thread.segments.last().copied() else {
			out.push(thread.state);
			if !collect_all || out.len() >= RESULT_LIMIT {
				return Ok(out);
			}
			continue;
		};
		let node = &segment.nodes[segment.index];
		let mut rest = thread.segments.clone();
		if let Some(last) = rest.last_mut() {
			last.index += 1;
		}
		match node {
			Node::Literal { value, case } => {
				if matches_literal(subject.unit(thread.state.pos), *value, *case) {
					let mut state = thread.state;
					state.pos += 1;
					stack.push(Thread { segments: rest, state });
				}
			},
			Node::NotLiteral { value, case } => {
				if thread.state.pos < subject.len()
					&& !matches_literal(subject.unit(thread.state.pos), *value, *case)
				{
					let mut state = thread.state;
					state.pos += 1;
					stack.push(Thread { segments: rest, state });
				}
			},
			Node::Any { all } => {
				if let Some(unit) = subject.unit(thread.state.pos) {
					if *all || unit != 10 {
						let mut state = thread.state;
						state.pos += 1;
						stack.push(Thread { segments: rest, state });
					}
				}
			},
			Node::At(at) => {
				if at_matches(*at, subject, thread.state.pos) {
					stack.push(Thread { segments: rest, state: thread.state });
				}
			},
			Node::In { set, case } => {
				if let Some(unit) = subject.unit(thread.state.pos) {
					if set.matches(unit, *case) {
						let mut state = thread.state;
						state.pos += 1;
						stack.push(Thread { segments: rest, state });
					}
				}
			},
			Node::Mark(mark) => {
				let mut state = thread.state;
				if *mark < state.marks.len() {
					state.marks[*mark] = Some(state.pos);
					if mark % 2 == 1 {
						state.lastindex = Some(mark / 2 + 1);
					}
				}
				stack.push(Thread { segments: rest, state });
			},
			Node::GroupRef { group, case } => {
				if let Some(state) = match_group_ref(subject, thread.state, *group, *case) {
					stack.push(Thread { segments: rest, state });
				}
			},
			Node::GroupRefExists { group, yes, no } => {
				let chosen = if group_is_matched(&thread.state, *group) {
					yes
				} else {
					no
				};
				let mut segments = rest;
				segments.push(Segment { nodes: chosen, index: 0 });
				stack.push(Thread { segments, state: thread.state });
			},
			Node::Branch(branches) => {
				for branch in branches.iter().rev() {
					let mut segments = rest.clone();
					segments.push(Segment { nodes: branch, index: 0 });
					stack.push(Thread { segments, state: thread.state.clone() });
				}
			},
			Node::Repeat { body, min, max, kind } => match kind {
				RepeatKind::Possessive => {
					// Possessive `X{m,n}+` is an atomic wrapper over the greedy
					// repeat (CPython/Python docs: `x*+` == `(?>x*)`): commit to
					// the greedy repeat's most-preferred (forward-march) result
					// and never backtrack into it.
					if let Some(state) =
						repeat_candidates(body, subject, thread.state, *min, *max, RepeatKind::Greedy)?
							.into_iter()
							.next()
					{
						stack.push(Thread { segments: rest, state });
					}
				},
				RepeatKind::Greedy | RepeatKind::Lazy => {
					// `repeat_candidates` returns candidates most-preferred
					// first; push reversed so the preferred one lands on top of
					// the LIFO backtracking stack and is tried first.
					for state in repeat_candidates(body, subject, thread.state, *min, *max, *kind)?
						.into_iter()
						.rev()
					{
						stack.push(Thread { segments: rest.clone(), state });
					}
				},
			},
			Node::Assert { positive, width, body } => {
				if thread.state.pos >= *width {
					let mut assert_state = thread.state.clone();
					assert_state.pos -= *width;
					let matches = execute(body, subject, assert_state, true)?;
					if *positive {
						for mut state in matches.into_iter().rev() {
							state.pos = thread.state.pos;
							stack.push(Thread { segments: rest.clone(), state });
						}
					} else if matches.is_empty() {
						stack.push(Thread { segments: rest, state: thread.state });
					}
				} else if !*positive {
					stack.push(Thread { segments: rest, state: thread.state });
				}
			},
			Node::Atomic(body) => {
				let mut matches = execute(body, subject, thread.state, false)?;
				if let Some(state) = matches.pop() {
					stack.push(Thread { segments: rest, state });
				}
			},
		}
	}
	Ok(out)
}

enum RepeatOneStep {
	Matched(MatchState),
	Failed,
	Unsupported,
}

fn repeat_single_unit_candidates(
	body: &[Node],
	subject: &SubjectData,
	state: MatchState,
	min: usize,
	max: Option<usize>,
	kind: RepeatKind,
) -> Option<Vec<MatchState>> {
	let cap = max.unwrap_or(usize::MAX);
	let lazy = matches!(kind, RepeatKind::Lazy);
	let mut current = state;
	let mut count = 0_usize;
	let mut out = Vec::new();
	if min == 0 {
		out.push(current.clone());
		if lazy && out.len() >= RESULT_LIMIT {
			return Some(out);
		}
	}
	while count < cap {
		let next = match repeat_single_unit_step(body, subject, current.clone()) {
			RepeatOneStep::Matched(next) => next,
			RepeatOneStep::Failed => break,
			RepeatOneStep::Unsupported => return None,
		};
		if next.pos == current.pos {
			return None;
		}
		count += 1;
		current = next;
		if count >= min {
			out.push(current.clone());
			if lazy && out.len() >= RESULT_LIMIT {
				return Some(out);
			}
		}
	}
	if count < min {
		return Some(Vec::new());
	}
	if !lazy {
		out.reverse();
		out.truncate(RESULT_LIMIT);
	}
	Some(out)
}

fn repeat_single_unit_step(
	body: &[Node],
	subject: &SubjectData,
	mut state: MatchState,
) -> RepeatOneStep {
	let mut consumed = false;
	for node in body {
		match node {
			Node::Mark(mark) => {
				if *mark < state.marks.len() {
					state.marks[*mark] = Some(state.pos);
					if mark % 2 == 1 {
						state.lastindex = Some(mark / 2 + 1);
					}
				}
			},
			Node::Literal { value, case } => {
				if consumed {
					return RepeatOneStep::Unsupported;
				}
				let Some(unit) = subject.unit(state.pos) else {
					return RepeatOneStep::Failed;
				};
				if !matches_literal(Some(unit), *value, *case) {
					return RepeatOneStep::Failed;
				}
				state.pos += 1;
				consumed = true;
			},
			Node::NotLiteral { value, case } => {
				if consumed {
					return RepeatOneStep::Unsupported;
				}
				let Some(unit) = subject.unit(state.pos) else {
					return RepeatOneStep::Failed;
				};
				if matches_literal(Some(unit), *value, *case) {
					return RepeatOneStep::Failed;
				}
				state.pos += 1;
				consumed = true;
			},
			Node::Any { all } => {
				if consumed {
					return RepeatOneStep::Unsupported;
				}
				let Some(unit) = subject.unit(state.pos) else {
					return RepeatOneStep::Failed;
				};
				if !*all && unit == 10 {
					return RepeatOneStep::Failed;
				}
				state.pos += 1;
				consumed = true;
			},
			Node::In { set, case } => {
				if consumed {
					return RepeatOneStep::Unsupported;
				}
				let Some(unit) = subject.unit(state.pos) else {
					return RepeatOneStep::Failed;
				};
				if !set.matches(unit, *case) {
					return RepeatOneStep::Failed;
				}
				state.pos += 1;
				consumed = true;
			},
			_ => return RepeatOneStep::Unsupported,
		}
	}
	if consumed {
		RepeatOneStep::Matched(state)
	} else {
		RepeatOneStep::Unsupported
	}
}

fn repeat_candidates(
	body: &[Node],
	subject: &SubjectData,
	state: MatchState,
	min: usize,
	max: Option<usize>,
	kind: RepeatKind,
) -> Result<Vec<MatchState>, Error> {
	if let Some(candidates) =
		repeat_single_unit_candidates(body, subject, state.clone(), min, max, kind)
	{
		return Ok(candidates);
	}
	// Preference-ordered depth-first enumeration of repetition end states:
	// index 0 is the match the backtracking engine tries first — greedy prefers
	// more repetitions (forward-march longest), lazy prefers fewer.  Emitted
	// iteratively through an explicit work stack so deep repetitions (`.*` over
	// a long input) cannot overflow the native stack.  `execute(body, .., true)`
	// already yields body matches in preference order, so visiting them
	// left-to-right and interleaving an `Emit` for the current stop point
	// reproduces CPython's nested backtracking order.
	enum Work {
		Visit { count: usize, state: MatchState },
		Emit(MatchState),
	}
	let lazy = matches!(kind, RepeatKind::Lazy);
	let cap = max.unwrap_or_else(|| {
		subject
			.len()
			.saturating_sub(state.pos)
			.saturating_add(min)
			.saturating_add(1)
	});
	let mut out = Vec::new();
	let mut seen = HashSet::new();
	let mut work = vec![Work::Visit { count: 0, state }];
	let mut steps = 0usize;
	while let Some(item) = work.pop() {
		steps += 1;
		if steps > STEP_LIMIT {
			return Err(Error::ExecutionLimit);
		}
		match item {
			Work::Emit(state) => {
				out.push(state);
				if out.len() >= RESULT_LIMIT {
					return Ok(out);
				}
			},
			Work::Visit { count, state } => {
				let body_matches = if count < cap {
					execute(body, subject, state.clone(), true)?
				} else {
					Vec::new()
				};
				// Classify each body match (kept in preference order): a deeper
				// repetition (advancing, or an empty match while still below
				// `min`, which still counts toward it) versus a trailing empty
				// iteration.  Once `min` is met CPython performs exactly one
				// empty iteration — updating captures — then stops, so an empty
				// match becomes a terminal candidate rather than a recursion.
				enum Step {
					Recurse(MatchState),
					Trailing(MatchState),
				}
				let mut steps_out = Vec::new();
				for next in body_matches {
					if next.pos == state.pos && count >= min {
						steps_out.push(Step::Trailing(next));
						continue;
					}
					let key = (count + 1, next.pos, next.marks.clone(), next.lastindex);
					if seen.insert(key) {
						steps_out.push(Step::Recurse(next));
					}
				}
				let emit_self = count >= min;
				if lazy {
					// Lazy prefers the fewest repetitions: bare stop first, then
					// the body steps in preference order.
					for step in steps_out.into_iter().rev() {
						match step {
							Step::Recurse(s) => work.push(Work::Visit { count: count + 1, state: s }),
							Step::Trailing(s) => work.push(Work::Emit(s)),
						}
					}
					if emit_self {
						work.push(Work::Emit(state));
					}
				} else {
					// Greedy prefers more repetitions: body steps first, then the
					// bare stop.
					if emit_self {
						work.push(Work::Emit(state));
					}
					for step in steps_out.into_iter().rev() {
						match step {
							Step::Recurse(s) => work.push(Work::Visit { count: count + 1, state: s }),
							Step::Trailing(s) => work.push(Work::Emit(s)),
						}
					}
				}
			},
		}
	}
	Ok(out)
}

impl Charset {
	fn matches(&self, unit: u32, case: CaseMode) -> bool {
		let folded = fold_unit(unit, case);
		let mut matched = false;
		for item in &self.items {
			matched |= match item {
				SetItem::Literal(value) => folded == fold_unit(*value, case),
				SetItem::Range(start, end) => {
					let lo = fold_unit(*start, case);
					let hi = fold_unit(*end, case);
					(lo..=hi).contains(&folded) || (*start..=*end).contains(&unit)
				},
				SetItem::RangeUnicodeIgnore(start, end) => {
					let lower = fold_unit(unit, CaseMode::Unicode);
					let upper = char::from_u32(unit)
						.and_then(|ch| ch.to_uppercase().next())
						.map_or(unit, |ch| ch as u32);
					(*start..=*end).contains(&lower)
						|| (*start..=*end).contains(&upper)
						|| (*start..=*end).contains(&unit)
				},
				SetItem::Bitmap(words) => bitmap_contains(words, folded),
				SetItem::BigCharset(words) => bigcharset_contains(words, folded),
				SetItem::Category(category) => category_matches(*category, unit),
			};
			if matched {
				break;
			}
		}
		if self.negated { !matched } else { matched }
	}
}

fn repeat_max_to_usize(value: u32) -> usize {
	if value == MAXREPEAT {
		usize::MAX
	} else {
		as_usize(value)
	}
}

fn max_repeat(value: u32) -> Option<usize> {
	if value == MAXREPEAT {
		None
	} else {
		Some(repeat_max_to_usize(value))
	}
}

fn literal_case(op: u32) -> CaseMode {
	match op {
		LITERAL_IGNORE | NOT_LITERAL_IGNORE => CaseMode::Ascii,
		LITERAL_LOC_IGNORE | NOT_LITERAL_LOC_IGNORE => CaseMode::Locale,
		LITERAL_UNI_IGNORE | NOT_LITERAL_UNI_IGNORE => CaseMode::Unicode,
		_ => CaseMode::Exact,
	}
}

fn in_case(op: u32) -> CaseMode {
	match op {
		IN_IGNORE => CaseMode::Ascii,
		IN_LOC_IGNORE => CaseMode::Locale,
		IN_UNI_IGNORE => CaseMode::Unicode,
		_ => CaseMode::Exact,
	}
}

fn groupref_case(op: u32) -> CaseMode {
	match op {
		GROUPREF_IGNORE => CaseMode::Ascii,
		GROUPREF_LOC_IGNORE => CaseMode::Locale,
		GROUPREF_UNI_IGNORE => CaseMode::Unicode,
		_ => CaseMode::Exact,
	}
}

fn matches_literal(unit: Option<u32>, value: u32, case: CaseMode) -> bool {
	unit.is_some_and(|actual| fold_unit(actual, case) == fold_unit(value, case))
}

fn fold_unit(unit: u32, case: CaseMode) -> u32 {
	match case {
		CaseMode::Exact => unit,
		CaseMode::Ascii | CaseMode::Locale => {
			if (b'A' as u32..=b'Z' as u32).contains(&unit) {
				unit + 32
			} else {
				unit
			}
		},
		CaseMode::Unicode => char::from_u32(unit)
			.and_then(|ch| ch.to_lowercase().next())
			.map_or(unit, |ch| ch as u32),
	}
}

fn match_group_ref(
	subject: &SubjectData,
	mut state: MatchState,
	group: usize,
	case: CaseMode,
) -> Option<MatchState> {
	let mark = group.checked_mul(2)?;
	let start = *state.marks.get(mark)?.as_ref()?;
	let end = *state.marks.get(mark + 1)?.as_ref()?;
	let len = end.checked_sub(start)?;
	if state.pos + len > subject.len() {
		return None;
	}
	for offset in 0..len {
		let expected = subject.unit(start + offset)?;
		let actual = subject.unit(state.pos + offset)?;
		if fold_unit(expected, case) != fold_unit(actual, case) {
			return None;
		}
	}
	state.pos += len;
	Some(state)
}

fn group_is_matched(state: &MatchState, group: usize) -> bool {
	let mark = group * 2;
	matches!((state.marks.get(mark), state.marks.get(mark + 1)), (Some(Some(_)), Some(Some(_))))
}

fn at_matches(at: u32, subject: &SubjectData, pos: usize) -> bool {
	match at {
		AT_BEGINNING | AT_BEGINNING_STRING => pos == 0,
		AT_BEGINNING_LINE => pos == 0 || subject.unit(pos.wrapping_sub(1)) == Some(10),
		AT_END => pos == subject.len() || (pos + 1 == subject.len() && subject.unit(pos) == Some(10)),
		AT_END_LINE => pos == subject.len() || subject.unit(pos) == Some(10),
		AT_END_STRING => pos == subject.len(),
		AT_BOUNDARY | AT_LOC_BOUNDARY | AT_UNI_BOUNDARY => boundary(subject, pos, at),
		AT_NON_BOUNDARY | AT_LOC_NON_BOUNDARY | AT_UNI_NON_BOUNDARY => !boundary(subject, pos, at),
		_ => false,
	}
}

fn boundary(subject: &SubjectData, pos: usize, at: u32) -> bool {
	let before = pos.checked_sub(1).and_then(|index| subject.unit(index));
	let after = subject.unit(pos);
	word_for_boundary(before, at) != word_for_boundary(after, at)
}

fn word_for_boundary(unit: Option<u32>, at: u32) -> bool {
	let Some(unit) = unit else {
		return false;
	};
	match at {
		AT_UNI_BOUNDARY | AT_UNI_NON_BOUNDARY => category_matches(CATEGORY_UNI_WORD, unit),
		_ => category_matches(CATEGORY_WORD, unit),
	}
}

fn category_matches(category: u32, unit: u32) -> bool {
	let yes = match category {
		CATEGORY_DIGIT => is_ascii_digit(unit),
		CATEGORY_SPACE => is_ascii_space(unit),
		CATEGORY_WORD | CATEGORY_LOC_WORD => is_ascii_word(unit),
		CATEGORY_LINEBREAK => unit == 10,
		CATEGORY_UNI_DIGIT => char::from_u32(unit).is_some_and(char::is_numeric),
		CATEGORY_UNI_SPACE => char::from_u32(unit).is_some_and(char::is_whitespace),
		CATEGORY_UNI_WORD => char::from_u32(unit).is_some_and(|ch| ch == '_' || ch.is_alphanumeric()),
		CATEGORY_UNI_LINEBREAK => matches!(unit, 10 | 11 | 12 | 13 | 0x85 | 0x2028 | 0x2029),
		CATEGORY_NOT_DIGIT => !is_ascii_digit(unit),
		CATEGORY_NOT_SPACE => !is_ascii_space(unit),
		CATEGORY_NOT_WORD | CATEGORY_LOC_NOT_WORD => !is_ascii_word(unit),
		CATEGORY_NOT_LINEBREAK => unit != 10,
		CATEGORY_UNI_NOT_DIGIT => !char::from_u32(unit).is_some_and(char::is_numeric),
		CATEGORY_UNI_NOT_SPACE => !char::from_u32(unit).is_some_and(char::is_whitespace),
		CATEGORY_UNI_NOT_WORD => {
			!char::from_u32(unit).is_some_and(|ch| ch == '_' || ch.is_alphanumeric())
		},
		CATEGORY_UNI_NOT_LINEBREAK => !matches!(unit, 10 | 11 | 12 | 13 | 0x85 | 0x2028 | 0x2029),
		_ => false,
	};
	yes
}

fn is_ascii_digit(unit: u32) -> bool {
	(b'0' as u32..=b'9' as u32).contains(&unit)
}

fn is_ascii_word(unit: u32) -> bool {
	is_ascii_digit(unit)
		|| (b'a' as u32..=b'z' as u32).contains(&unit)
		|| (b'A' as u32..=b'Z' as u32).contains(&unit)
		|| unit == b'_' as u32
}

fn is_ascii_space(unit: u32) -> bool {
	matches!(unit, 9 | 10 | 11 | 12 | 13 | 32)
}

fn bitmap_contains(words: &[u32], unit: u32) -> bool {
	if unit >= 256 {
		return false;
	}
	let index = as_usize(unit / 32);
	let bit = unit % 32;
	words
		.get(index)
		.is_some_and(|word| (word & (1u32 << bit)) != 0)
}

fn bigcharset_contains(words: &[u32], unit: u32) -> bool {
	if unit >= 65_536 || words.is_empty() {
		return false;
	}
	let blocks = as_usize(words[0]);
	if words.len() < 65 + blocks * 8 {
		return false;
	}
	let high = as_usize(unit >> 8);
	let map_word = words[1 + high / 4];
	let block = as_usize((map_word >> ((high % 4) * 8)) & 0xff);
	if block >= blocks {
		return false;
	}
	let low = unit & 0xff;
	let word = words[65 + block * 8 + as_usize(low / 32)];
	(word & (1u32 << (low % 32))) != 0
}

fn need(code: &[u32], pc: usize, needed: usize) -> Result<(), Error> {
	if pc + needed <= code.len() {
		Ok(())
	} else {
		Err(Error::Truncated { pc, needed })
	}
}

fn checked_skip_end(pc: usize, skip: u32) -> Result<usize, Error> {
	pc.checked_add(1)
		.and_then(|base| base.checked_add(as_usize(skip)))
		.ok_or(Error::InvalidSkip { pc, skip })
}

fn as_usize(value: u32) -> usize {
	value as usize
}

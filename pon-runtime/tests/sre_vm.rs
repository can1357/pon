use std::collections::BTreeMap;

// The VM keeps its full API for the in-crate `_sre` wrapper; this standalone
// include only exercises the fixture-facing subset.
#[allow(dead_code)]
#[path = "../src/native/sre/vm.rs"]
mod sre;

const FIXTURES: &str = include_str!("../src/native/sre/fixtures.json");

#[derive(Clone, Debug, PartialEq)]
enum Json {
    Null,
    Bool(bool),
    Number(i64),
    String(String),
    Array(Vec<Json>),
    Object(BTreeMap<String, Json>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct MatchRecord {
    span: (usize, usize),
    groups: Vec<Option<(usize, usize)>>,
    lastindex: Option<usize>,
    lastgroup: Option<String>,
}

struct Parser<'a> {
    input: &'a str,
    pos: usize,
}

#[test]
fn sre_fixture_corpus() {
    let root = parse_json(FIXTURES);
    assert_eq!(num(root.get("magic")) as u32, sre::MAGIC);
    assert_eq!(num(root.get("codesize")) as usize, sre::getcodesize());
    assert_eq!(num(root.get("maxrepeat")) as u32, sre::MAXREPEAT);
    for case in root.get("cases").array() {
        run_case(case);
    }
}

#[test]
fn sre_surface_methods_basics() {
    let root = parse_json(FIXTURES);
    let case = root
        .get("cases")
        .array()
        .iter()
        .find(|case| case.get("name").string() == "sre_curated_037")
        .expect("curated named-group fixture exists");
    let pattern = compile_case(case);
    let search = pattern.search_str("xxabc-abc yy").unwrap().expect("search match");
    assert_eq!(search.group(0), Some(sre::MatchedValue::Str("abc-abc".to_owned())));
    assert_eq!(search.group_name("word"), Some(sre::MatchedValue::Str("abc".to_owned())));
    assert_eq!(search.groups(), vec![Some(sre::MatchedValue::Str("abc".to_owned()))]);
    assert_eq!(search.groupdict().get("word"), Some(&Some(sre::MatchedValue::Str("abc".to_owned()))));
    assert_eq!(search.start(0), Some(2));
    assert_eq!(search.end(0), Some(9));
    assert_eq!(search.span(1).flatten(), Some((2, 5)));
    assert_eq!(search.lastindex(), Some(1));
    assert_eq!(search.lastgroup(), Some("word"));

    let repeat_case = root
        .get("cases")
        .array()
        .iter()
        .find(|case| case.get("pattern").as_str() == Some("a+"))
        .expect("a+ fixture exists");
    let repeat = compile_case(repeat_case);
    assert_eq!(repeat.match_str("aaab").unwrap().and_then(|m| m.span(0).flatten()), Some((0, 3)));
    assert_eq!(repeat.fullmatch_str("aaa").unwrap().and_then(|m| m.span(0).flatten()), Some((0, 3)));
    assert_eq!(repeat.search_str("baacaa").unwrap().and_then(|m| m.span(0).flatten()), Some((1, 3)));
    assert_eq!(repeat.finditer_str("baacaa").unwrap().iter().map(|m| m.span(0).flatten().unwrap()).collect::<Vec<_>>(), vec![(1, 3), (4, 6)]);
    assert_eq!(repeat.findall_str("baacaa").unwrap(), vec![vec![Some(sre::MatchedValue::Str("aa".to_owned()))], vec![Some(sre::MatchedValue::Str("aa".to_owned()))]]);
    assert_eq!(repeat.split_str("baacaa").unwrap(), vec![sre::MatchedValue::Str("b".to_owned()), sre::MatchedValue::Str("c".to_owned()), sre::MatchedValue::Str(String::new())]);
    assert_eq!(repeat.sub_str("#", "baacaa").unwrap(), "b#c#");

    let bytes_case = root
        .get("cases")
        .array()
        .iter()
        .find(|case| case.get("kind").string() == "bytes" && case.get("pattern_bytes").array().starts_with(&[Json::Number(97), Json::Number(43)]))
        .expect("bytes a+ fixture exists");
    let bytes_pattern = compile_case(bytes_case);
    assert_eq!(bytes_pattern.search_bytes(b"xxaaab").unwrap().and_then(|m| m.span(0).flatten()), Some((2, 5)));
    assert_eq!(bytes_pattern.findall_bytes(b"baacaa").unwrap(), vec![vec![Some(sre::MatchedValue::Bytes(b"aa".to_vec()))], vec![Some(sre::MatchedValue::Bytes(b"aa".to_vec()))]]);
    assert_eq!(bytes_pattern.split_bytes(b"baacaa").unwrap(), vec![sre::MatchedValue::Bytes(b"b".to_vec()), sre::MatchedValue::Bytes(b"c".to_vec()), sre::MatchedValue::Bytes(Vec::new())]);
    assert_eq!(bytes_pattern.sub_bytes(b"#", b"baacaa").unwrap(), b"b#c#".to_vec());
}

fn run_case(case: &Json) {
    let name = case.get("name").string();
    let pattern = compile_case(case);
    let groups = num(case.get("groups")) as usize;
    let expect = case.get("expect");
    match case.get("kind").string() {
        "str" => {
            let subject = case.get("subject").string();
            assert_eq!(record(pattern.match_str(subject).unwrap().as_ref(), groups), expected(expect.get("match")), "{name} match");
            assert_eq!(record(pattern.search_str(subject).unwrap().as_ref(), groups), expected(expect.get("search")), "{name} search");
            assert_eq!(record(pattern.fullmatch_str(subject).unwrap().as_ref(), groups), expected(expect.get("fullmatch")), "{name} fullmatch");
            let actual = pattern.finditer_str(subject).unwrap();
            assert_eq!(actual.iter().map(|m| record(Some(m), groups).unwrap()).collect::<Vec<_>>(), expected_list(expect.get("finditer")), "{name} finditer");
        }
        "bytes" => {
            let subject = bytes(case.get("subject_bytes"));
            assert_eq!(record(pattern.match_bytes(&subject).unwrap().as_ref(), groups), expected(expect.get("match")), "{name} match");
            assert_eq!(record(pattern.search_bytes(&subject).unwrap().as_ref(), groups), expected(expect.get("search")), "{name} search");
            assert_eq!(record(pattern.fullmatch_bytes(&subject).unwrap().as_ref(), groups), expected(expect.get("fullmatch")), "{name} fullmatch");
            let actual = pattern.finditer_bytes(&subject).unwrap();
            assert_eq!(actual.iter().map(|m| record(Some(m), groups).unwrap()).collect::<Vec<_>>(), expected_list(expect.get("finditer")), "{name} finditer");
        }
        other => panic!("unknown subject kind {other}"),
    }
}

fn compile_case(case: &Json) -> sre::Pattern {
    let pattern = match case.get("pattern").as_str() {
        Some(text) => sre::PatternText::Str(text.to_owned()),
        None => sre::PatternText::Bytes(bytes(case.get("pattern_bytes"))),
    };
    let groupindex = case
        .get("groupindex")
        .object()
        .iter()
        .map(|(name, value)| (name.clone(), num(value) as usize))
        .collect::<BTreeMap<_, _>>();
    let indexgroup = case
        .get("indexgroup")
        .array()
        .iter()
        .map(|value| value.as_str().map(str::to_owned))
        .collect::<Vec<_>>();
    sre::compile_checked(
        sre::MAGIC,
        pattern,
        num(case.get("flags")) as u32,
        case.get("code").array().iter().map(|value| num(value) as u32).collect(),
        num(case.get("groups")) as usize,
        groupindex,
        indexgroup,
    )
    .unwrap()
}

fn record(matched: Option<&sre::Match>, groups: usize) -> Option<MatchRecord> {
    let matched = matched?;
    Some(MatchRecord {
        span: matched.span(0).flatten().unwrap(),
        groups: (1..=groups).map(|index| matched.span(index).flatten()).collect(),
        lastindex: matched.lastindex(),
        lastgroup: matched.lastgroup().map(str::to_owned),
    })
}

fn expected(value: &Json) -> Option<MatchRecord> {
    if matches!(value, Json::Null) {
        return None;
    }
    Some(MatchRecord {
        span: pair(value.get("span")),
        groups: value
            .get("groups")
            .array()
            .iter()
            .map(|group| if matches!(group, Json::Null) { None } else { Some(pair(group)) })
            .collect(),
        lastindex: opt_usize(value.get("lastindex")),
        lastgroup: value.get("lastgroup").as_str().map(str::to_owned),
    })
}

fn expected_list(value: &Json) -> Vec<MatchRecord> {
    value.array().iter().map(|item| expected(item).expect("finditer entries are matches")).collect()
}

fn pair(value: &Json) -> (usize, usize) {
    let array = value.array();
    (num(&array[0]) as usize, num(&array[1]) as usize)
}

fn opt_usize(value: &Json) -> Option<usize> {
    if matches!(value, Json::Null) { None } else { Some(num(value) as usize) }
}

fn bytes(value: &Json) -> Vec<u8> {
    value.array().iter().map(|value| num(value) as u8).collect()
}

fn num(value: &Json) -> i64 {
    match value {
        Json::Number(number) => *number,
        other => panic!("expected number, got {other:?}"),
    }
}

fn parse_json(input: &str) -> Json {
    let mut parser = Parser { input, pos: 0 };
    let value = parser.value();
    parser.ws();
    assert_eq!(parser.pos, input.len());
    value
}

impl<'a> Parser<'a> {
    fn value(&mut self) -> Json {
        self.ws();
        match self.peek() {
            Some(b'n') => { self.expect("null"); Json::Null }
            Some(b't') => { self.expect("true"); Json::Bool(true) }
            Some(b'f') => { self.expect("false"); Json::Bool(false) }
            Some(b'\"') => Json::String(self.string()),
            Some(b'[') => self.array(),
            Some(b'{') => self.object(),
            Some(b'-' | b'0'..=b'9') => self.number(),
            other => panic!("unexpected JSON byte {other:?} at {}", self.pos),
        }
    }

    fn array(&mut self) -> Json {
        self.byte(b'[');
        let mut values = Vec::new();
        self.ws();
        if self.peek() == Some(b']') {
            self.pos += 1;
            return Json::Array(values);
        }
        loop {
            values.push(self.value());
            self.ws();
            match self.next() {
                Some(b',') => {}
                Some(b']') => break,
                other => panic!("unexpected array delimiter {other:?}"),
            }
        }
        Json::Array(values)
    }

    fn object(&mut self) -> Json {
        self.byte(b'{');
        let mut values = BTreeMap::new();
        self.ws();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            return Json::Object(values);
        }
        loop {
            self.ws();
            let key = self.string();
            self.ws();
            self.byte(b':');
            values.insert(key, self.value());
            self.ws();
            match self.next() {
                Some(b',') => {}
                Some(b'}') => break,
                other => panic!("unexpected object delimiter {other:?}"),
            }
        }
        Json::Object(values)
    }

    fn string(&mut self) -> String {
        self.byte(b'\"');
        let mut out = String::new();
        while self.pos < self.input.len() {
            let ch = self.input[self.pos..].chars().next().unwrap();
            self.pos += ch.len_utf8();
            match ch {
                '\"' => return out,
                '\\' => match self.next().expect("escape byte") {
                    b'\"' => out.push('\"'),
                    b'\\' => out.push('\\'),
                    b'/' => out.push('/'),
                    b'b' => out.push('\u{0008}'),
                    b'f' => out.push('\u{000c}'),
                    b'n' => out.push('\n'),
                    b'r' => out.push('\r'),
                    b't' => out.push('\t'),
                    b'u' => out.push(self.unicode_escape()),
                    other => panic!("bad escape {other}"),
                },
                other => out.push(other),
            }
        }
        panic!("unterminated JSON string")
    }

    fn unicode_escape(&mut self) -> char {
        let mut value = 0u32;
        for _ in 0..4 {
            let byte = self.next().expect("hex byte");
            value = (value << 4)
                | match byte {
                    b'0'..=b'9' => u32::from(byte - b'0'),
                    b'a'..=b'f' => u32::from(byte - b'a' + 10),
                    b'A'..=b'F' => u32::from(byte - b'A' + 10),
                    other => panic!("bad hex byte {other}"),
                };
        }
        char::from_u32(value).expect("valid scalar fixture escape")
    }

    fn number(&mut self) -> Json {
        let start = self.pos;
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        while matches!(self.peek(), Some(b'0'..=b'9')) {
            self.pos += 1;
        }
        Json::Number(self.input[start..self.pos].parse().unwrap())
    }

    fn ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\n' | b'\r' | b'\t')) {
            self.pos += 1;
        }
    }

    fn expect(&mut self, expected: &str) {
        assert!(self.input[self.pos..].starts_with(expected));
        self.pos += expected.len();
    }

    fn byte(&mut self, expected: u8) {
        assert_eq!(self.next(), Some(expected));
    }

    fn next(&mut self) -> Option<u8> {
        let byte = self.peek()?;
        self.pos += 1;
        Some(byte)
    }

    fn peek(&self) -> Option<u8> {
        self.input.as_bytes().get(self.pos).copied()
    }
}

impl Json {
    fn get(&self, key: &str) -> &Json {
        self.object().get(key).unwrap_or_else(|| panic!("missing key {key}"))
    }

    fn object(&self) -> &BTreeMap<String, Json> {
        match self {
            Json::Object(value) => value,
            other => panic!("expected object, got {other:?}"),
        }
    }

    fn array(&self) -> &[Json] {
        match self {
            Json::Array(value) => value,
            other => panic!("expected array, got {other:?}"),
        }
    }

    fn string(&self) -> &str {
        self.as_str().expect("expected string")
    }

    fn as_str(&self) -> Option<&str> {
        match self {
            Json::String(value) => Some(value),
            Json::Null => None,
            _ => None,
        }
    }
}

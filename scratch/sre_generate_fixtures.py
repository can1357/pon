#!/usr/bin/env python3.14
"""Regenerate the isolated `_sre` VM oracle fixtures.

Command, from the repository root:

    python3.14 scratch/sre_generate_fixtures.py

The script loads the vendored CPython 3.14 `re` package, extracts bytecode from
`re._compiler._code()`, and records CPython's observable match/search/fullmatch
and iterator spans for the Rust VM tests.
"""

from __future__ import annotations

import json
import pathlib
import sys
from typing import Any

ROOT = pathlib.Path(__file__).resolve().parents[1]
VENDOR_LIB = ROOT / "pon-conformance" / "vendor" / "cpython-3.14" / "Lib"
OUT = ROOT / "pon-runtime" / "src" / "native" / "sre" / "fixtures.json"

sys.path.insert(0, str(VENDOR_LIB))

import re  # noqa: E402
from re import _compiler, _constants, _parser  # noqa: E402


def _code_for(pattern: str | bytes, flags: int) -> tuple[list[int], int, int, dict[str, int], list[str | None]]:
    parsed = _parser.parse(pattern, flags)
    code = [int(word) for word in _compiler._code(parsed, flags)]
    groupindex = dict(parsed.state.groupdict)
    indexgroup: list[str | None] = [None] * parsed.state.groups
    for name, index in groupindex.items():
        indexgroup[index] = name
    return code, flags | parsed.state.flags, parsed.state.groups - 1, groupindex, indexgroup


def _span(match: re.Match[Any] | None, groups: int) -> dict[str, Any] | None:
    if match is None:
        return None
    group_spans: list[list[int] | None] = []
    for index in range(1, groups + 1):
        start, end = match.span(index)
        group_spans.append(None if start < 0 else [start, end])
    return {
        "span": list(match.span(0)),
        "groups": group_spans,
        "lastindex": match.lastindex,
        "lastgroup": match.lastgroup,
    }


def _subject_field(subject: str | bytes) -> dict[str, Any]:
    if isinstance(subject, bytes):
        return {"kind": "bytes", "subject_bytes": list(subject)}
    return {"kind": "str", "subject": subject}


def _pattern_field(pattern: str | bytes) -> dict[str, Any]:
    if isinstance(pattern, bytes):
        return {"pattern_bytes": list(pattern), "pattern": None}
    return {"pattern": pattern}


def _fixture(name: str, pattern: str | bytes, subject: str | bytes, flags: int = 0) -> dict[str, Any] | None:
    try:
        code, combined_flags, groups, groupindex, indexgroup = _code_for(pattern, flags)
        compiled = re.compile(pattern, flags)
    except re.error:
        return None
    case: dict[str, Any] = {
        "name": name,
        "flags": combined_flags,
        "code": code,
        "groups": groups,
        "groupindex": groupindex,
        "indexgroup": indexgroup,
        **_pattern_field(pattern),
        **_subject_field(subject),
        "expect": {
            "match": _span(compiled.match(subject), groups),
            "search": _span(compiled.search(subject), groups),
            "fullmatch": _span(compiled.fullmatch(subject), groups),
            "finditer": [_span(m, groups) for m in compiled.finditer(subject)],
        },
    }
    return case


def _load_re_tests() -> list[tuple[str, str]]:
    ns = runpy_namespace(VENDOR_LIB / "test" / "re_tests.py")
    succeed, fail, _syntax_error = 0, 1, 2
    out: list[tuple[str, str]] = []
    for entry in ns["tests"]:
        pattern, subject, result = entry[:3]
        if result not in (succeed, fail):
            continue
        if not isinstance(pattern, str) or not isinstance(subject, str):
            continue
        out.append((pattern, subject))
    return out


def runpy_namespace(path: pathlib.Path) -> dict[str, Any]:
    code = compile(path.read_text(), str(path), "exec")
    ns: dict[str, Any] = {"__file__": str(path), "__name__": "_sre_fixture_re_tests"}
    exec(code, ns)
    return ns


def main() -> None:
    curated: list[tuple[str | bytes, str | bytes, int]] = [
        ("", "", 0),
        ("abc", "xabcy", 0),
        ("^abc$", "abc", 0),
        ("(?m)^abc$", "x\nabc\ny", 0),
        (r"\Aabc\Z", "abc", 0),
        ("a.c", "a\nc axc", 0),
        ("(?s)a.c", "a\nc", 0),
        ("ab|cd", "xxcd", 0),
        ("a|bc|def", "zzdef", 0),
        ("(foo|bar|baz)qux", "xxbarqux", 0),
        ("[a-z]+", "123abcXYZ", 0),
        ("[^a]+", "aaabbb", 0),
        (r"\d+", "abc١٢3", 0),
        (r"(?a)\w+", "éabc_123", 0),
        (r"\s+", "a \t\nb", 0),
        (r"\ba\b", "-a-", 0),
        (r"\By\B", "xyz", 0),
        ("a*", "aaab", 0),
        ("a*?b", "aaab", 0),
        ("a+", "baaac", 0),
        ("a??", "aaa", 0),
        ("a{2,4}b", "aaaaab", 0),
        ("a{2,4}?b", "aaaaab", 0),
        ("(ab)*", "ababx", 0),
        ("(ab)+?c", "abababc", 0),
        ("(ab|cd)*?e", "abcdabe", 0),
        ("a{2,3}+a", "aaaa", 0),
        ("(ab){2,3}+ab", "abababab", 0),
        ("(?>a*)a", "aaa", 0),
        ("(?=a)a", "a", 0),
        ("(?!a).", "ba", 0),
        ("(?<=a)b", "ab", 0),
        ("(?<!a)b", "cb", 0),
        ("(a)\\1", "aa", 0),
        ("(?i)(a)\\1", "aA", 0),
        ("(a)?(?(1)b|c)", "ab", 0),
        ("(a)?(?(1)b|c)", "c", 0),
        ("(?P<word>[A-Za-z]+)-(?P=word)", "abc-abc", 0),
        ("(?i)abc", "xxAbC", 0),
        ("(?a)(?i)abc", "xxAbC", 0),
        ("(?i)[A-Z]+", "éAbÇ", 0),
        ("[Ā-ſ]+", "xĀĲy", 0),
        ("(?i)[Ā-ſ]+", "xāĳy", 0),
        ("[\\u0100-\\u017f]+", "xĀĲy", 0),
        ("[\\U00010000-\\U00010010]+", "x\U00010005y", 0),
        ("(?a)(?i)[^a]", "bbb", 0),
        ("(?i)[𐐀-𐐧]", "x𐐨y", 0),
        (b"a+", b"xxaaab", 0),
        (br"[A-Z]+", b"abCDEf", 0),
        (br"(?i)[a-z]+", b"12AbC", 0),
        (br"(a+)\1", b"aaaa", 0),
        (br"\b[a-z]+\b", b"--abc--", 0),
        (br"(?s)a.b", b"a\nb", 0),
        (br"(?L)(?i)[a-z]+", b"12AbC", 0),
    ]

    lifted: list[tuple[str | bytes, str | bytes, int]] = [(pattern, subject, 0) for pattern, subject in _load_re_tests()[:140]]
    seen: set[tuple[str, str, int]] = set()
    cases: list[dict[str, Any]] = []
    for prefix, entries in (("curated", curated), ("re_tests", lifted)):
        for index, (pattern, subject, flags) in enumerate(entries):
            key = (repr(pattern), repr(subject), flags)
            if key in seen:
                continue
            seen.add(key)
            fixture = _fixture(f"sre_{prefix}_{index:03d}", pattern, subject, flags)
            if fixture is not None:
                cases.append(fixture)

    opnames = [
        "FAILURE", "SUCCESS", "ANY", "ANY_ALL", "ASSERT", "ASSERT_NOT", "AT", "BRANCH",
        "CATEGORY", "CHARSET", "BIGCHARSET", "GROUPREF", "GROUPREF_EXISTS", "IN", "INFO", "JUMP",
        "LITERAL", "MARK", "MAX_UNTIL", "MIN_UNTIL", "NOT_LITERAL", "NEGATE", "RANGE", "REPEAT",
        "REPEAT_ONE", "SUBPATTERN", "MIN_REPEAT_ONE", "ATOMIC_GROUP", "POSSESSIVE_REPEAT",
        "POSSESSIVE_REPEAT_ONE", "GROUPREF_IGNORE", "IN_IGNORE", "LITERAL_IGNORE", "NOT_LITERAL_IGNORE",
        "GROUPREF_LOC_IGNORE", "IN_LOC_IGNORE", "LITERAL_LOC_IGNORE", "NOT_LITERAL_LOC_IGNORE",
        "GROUPREF_UNI_IGNORE", "IN_UNI_IGNORE", "LITERAL_UNI_IGNORE", "NOT_LITERAL_UNI_IGNORE",
        "RANGE_UNI_IGNORE",
    ]
    hit = sorted({word for case in cases for word in case["code"] if 0 <= word < len(opnames)})
    root = {
        "magic": int(_constants.MAGIC),
        "codesize": int(_compiler._sre.CODESIZE),
        "maxrepeat": int(_constants.MAXREPEAT),
        "source": "python3.14 scratch/sre_generate_fixtures.py",
        "opcode_hits": {opnames[index]: index in hit for index in range(len(opnames))},
        "cases": cases,
    }
    OUT.write_text(json.dumps(root, ensure_ascii=False, indent=2, sort_keys=True) + "\n")
    print(f"wrote {OUT} ({len(cases)} cases)")


if __name__ == "__main__":
    main()

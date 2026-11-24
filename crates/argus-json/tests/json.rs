//! argus-json reads agent transcripts written by Node's `JSON.stringify` and writes
//! argus's own payloads. `write` follows `JSON.stringify`'s compact form, so a compact
//! foreign line survives `parse` then `write` byte for byte.

use argus_json::{Json, Number, parse, write};

fn num(lexeme: &str) -> Json {
    match parse(lexeme.as_bytes()).unwrap() {
        n @ Json::Num(_) => n,
        other => panic!("{lexeme} parsed as {other:?}"),
    }
}

#[test]
fn parses_every_kind_of_value() {
    let v = parse(br#" {"a": [null, true, false, 1, "x"], "b": {}, "c": []} "#).unwrap();
    assert_eq!(
        v,
        Json::Obj(vec![
            (
                "a".into(),
                Json::Arr(vec![
                    Json::Null,
                    Json::Bool(true),
                    Json::Bool(false),
                    num("1"),
                    Json::str("x"),
                ])
            ),
            ("b".into(), Json::Obj(vec![])),
            ("c".into(), Json::Arr(vec![])),
        ])
    );
}

#[test]
fn numbers_keep_their_lexeme() {
    for lexeme in [
        "0",
        "-0",
        "12",
        "-1.5e3",
        "1E+2",
        "0.000001",
        "18446744073709551616",
    ] {
        let v = num(lexeme);
        assert_eq!(write(&v), lexeme);
    }
    let Json::Num(n) = num("18446744073709551615") else {
        unreachable!()
    };
    assert_eq!(n.as_u64(), Some(u64::MAX));
    assert_eq!(
        Number::from_i64(-3),
        match num("-3") {
            Json::Num(n) => n,
            _ => unreachable!(),
        }
    );
}

#[test]
fn rejects_what_rfc_8259_rejects() {
    for bad in [
        &b""[..],
        b"01",
        b"1.",
        b".5",
        b"+1",
        b"1e",
        b"-",
        b"nul",
        b"[1,]",
        b"{\"a\":1,}",
        b"{a:1}",
        b"'x'",
        b"\"tab\there\"",
        b"\"\\x\"",
        b"\"\\u12\"",
        b"\"open",
        b"[1] 2",
        b"\xff",
        b"\"\xc3\"",
    ] {
        assert!(
            parse(bad).is_err(),
            "{:?} parsed",
            String::from_utf8_lossy(bad)
        );
    }
}

#[test]
fn string_escapes_decode() {
    let v = parse(br#""a\"b\\c\/d\b\f\n\r\t\u00e9\u3042\ud83d\ude00""#).unwrap();
    assert_eq!(v, Json::str("a\"b\\c/d\u{8}\u{c}\n\r\té\u{3042}😀"));
}

/// JavaScript strings may hold a lone surrogate, which Rust strings cannot. The value
/// keeps a replacement character; the verbatim bytes stay in the raw frame.
#[test]
fn a_lone_surrogate_becomes_a_replacement_character() {
    assert_eq!(parse(br#""x\ud800y""#).unwrap(), Json::str("x\u{fffd}y"));
    assert_eq!(parse(br#""\ude00""#).unwrap(), Json::str("\u{fffd}"));
}

#[test]
fn nesting_is_limited() {
    let deep = |n: usize| format!("{}{}", "[".repeat(n), "]".repeat(n));
    assert!(parse(deep(256).as_bytes()).is_ok());
    assert!(parse(deep(257).as_bytes()).is_err());
}

#[test]
fn errors_say_where() {
    let err = parse(b"[1, 2, ?]").unwrap_err();
    assert_eq!(err.at, 7);
}

#[test]
fn writes_like_json_stringify() {
    let v = Json::Obj(vec![
        (
            "z".into(),
            Json::str("\"\\\u{8}\u{c}\n\r\t\u{1}\u{1f}\u{7f}é😀/<"),
        ),
        (
            "a".into(),
            Json::Arr(vec![Json::Null, Json::Bool(true), num("1.0")]),
        ),
    ]);
    assert_eq!(
        write(&v),
        r#"{"z":"\"\\\b\f\n\r\t\u0001\u001f"#.to_owned() + "\u{7f}é😀/<\",\"a\":[null,true,1.0]}"
    );
}

/// Same idea as a property test: random values survive `write` then `parse`, and a
/// written value is a fixed point of `parse` then `write`.
#[test]
fn random_values_round_trip() {
    let mut rng = Rng(7);
    for _ in 0..2000 {
        let v = random_value(&mut rng, 0);
        let text = write(&v);
        let back = parse(text.as_bytes()).unwrap_or_else(|e| panic!("{text}: {e:?}"));
        assert_eq!(back, v, "{text}");
        assert_eq!(write(&back), text);
    }
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

fn random_string(rng: &mut Rng) -> String {
    const POOL: &[char] = &[
        'a', 'Z', '0', ' ', '"', '\\', '/', '\n', '\t', '\u{0}', '\u{1f}', 'é', 'あ', '😀',
        '\u{2028}',
    ];
    (0..rng.below(8))
        .map(|_| POOL[rng.below(POOL.len() as u64) as usize])
        .collect()
}

fn random_value(rng: &mut Rng, depth: u32) -> Json {
    let leaf = depth >= 4;
    match rng.below(if leaf { 5 } else { 7 }) {
        0 => Json::Null,
        1 => Json::Bool(rng.below(2) == 0),
        2 => {
            const LEXEMES: &[&str] = &[
                "0",
                "-1",
                "42",
                "3.25",
                "-0.5e-7",
                "1E9",
                "123456789012345678901234567890",
            ];
            num(LEXEMES[rng.below(LEXEMES.len() as u64) as usize])
        }
        3 | 4 => Json::Str(random_string(rng)),
        5 => Json::Arr(
            (0..rng.below(4))
                .map(|_| random_value(rng, depth + 1))
                .collect(),
        ),
        _ => Json::Obj(
            (0..rng.below(4))
                .map(|_| (random_string(rng), random_value(rng, depth + 1)))
                .collect(),
        ),
    }
}

/// Differential test against Node: every line of real Claude Code transcripts (written by
/// `JSON.stringify`) parses, and writing it back gives the same bytes. Reads files outside
/// the repo, so it runs only on request:
/// `ARGUS_TRANSCRIPTS=~/.claude/projects cargo nextest run -p argus-json --run-ignored all`
#[test]
#[ignore = "reads real transcripts named by ARGUS_TRANSCRIPTS"]
#[expect(
    clippy::disallowed_methods,
    reason = "an opt-in test reads the transcript root from ARGUS_TRANSCRIPTS"
)]
fn real_transcript_lines_round_trip_byte_for_byte() {
    let root = std::env::var_os("ARGUS_TRANSCRIPTS").expect("set ARGUS_TRANSCRIPTS");
    let mut files = vec![std::path::PathBuf::from(root)];
    let (mut lines, mut differ) = (0u64, Vec::new());
    while let Some(path) = files.pop() {
        if path.is_dir() {
            files.extend(std::fs::read_dir(&path).unwrap().map(|e| e.unwrap().path()));
            continue;
        }
        if path.extension().is_none_or(|e| e != "jsonl") {
            continue;
        }
        for (n, line) in std::fs::read(&path)
            .unwrap()
            .split(|&b| b == b'\n')
            .enumerate()
        {
            if line.is_empty() {
                continue;
            }
            lines += 1;
            let v = parse(line).unwrap_or_else(|e| panic!("{}:{}: {e:?}", path.display(), n + 1));
            if write(&v).as_bytes() != line {
                differ.push(format!("{}:{}", path.display(), n + 1));
            }
        }
    }
    eprintln!("{lines} lines, {} differ", differ.len());
    assert!(
        differ.is_empty(),
        "first differing lines: {:?}",
        &differ[..differ.len().min(5)]
    );
}

// ---------------------------------------------------------------- found by mutation testing

#[test]
fn numbers_read_as_integers_or_floats_without_losing_their_lexeme() {
    let as_num = |s: &str| match parse(s.as_bytes()).unwrap() {
        Json::Num(n) => n,
        other => panic!("{other:?}"),
    };
    assert_eq!(as_num("12").as_i64(), Some(12));
    assert_eq!(as_num("-7").as_i64(), Some(-7));
    assert_eq!(as_num("1.5").as_i64(), None);
    assert_eq!(as_num("1.5").as_f64(), Some(1.5));
    assert_eq!(as_num("-2e3").as_f64(), Some(-2000.0));
    assert_eq!(Number::from_i64(-3).as_i64(), Some(-3));
}

#[test]
fn fields_and_strings_are_read_only_from_what_holds_them() {
    let v = parse(br#"{"a":"x","b":1}"#).unwrap();
    assert_eq!(v.get("a").and_then(Json::as_str), Some("x"));
    assert_eq!(v.get("b").and_then(Json::as_str), None);
    assert_eq!(v.get("c"), None);
    assert_eq!(Json::Arr(vec![]).get("a"), None);
    assert_eq!(Json::str("y").as_str(), Some("y"));
    assert_eq!(Json::Null.as_str(), None);
}

#[test]
fn invalid_utf8_is_reported_where_it_starts() {
    let err = parse(b"\"ab\xffcd\"").unwrap_err();
    assert_eq!(err.at, 3);
    // After an escape, the position still counts from the start of the input.
    let err = parse(b"\"a\\n\xff\"").unwrap_err();
    assert_eq!(err.at, 4);
}

#[test]
fn a_space_is_written_as_itself_and_only_controls_are_escaped() {
    assert_eq!(write(&Json::str(" \u{1f}")), "\" \\u001f\"");
}

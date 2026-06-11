//! GS1 parsing + validation — element strings (FNC1 QR, GS1 `GenSpecs` §3/§7)
//! and Digital Link URIs (GS1 DL URI Syntax 1.6, Sunrise-2027 retail form).
//!
//! Scope honesty: this is a SYNTAX validator over the `GenSpecs` subset below
//! (the retail/product set), not a full `GenSpecs` engine. Unknown AIs are
//! flagged as issues and leave the data non-`conformant` — every issue
//! string names the violated criterion.

/// One parsed `(AI, value)` element string.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Gs1Element {
    /// Application Identifier digits as written (`"01"`, `"3103"`, …).
    pub ai: String,
    /// Raw value (not normalized).
    pub value: String,
}

/// Parse + validation outcome shared by both GS1 carriers.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct Gs1Data {
    pub elements: Vec<Gs1Element>,
    pub gtin: Option<String>,
    pub issues: Vec<String>,
}

/// ASCII Group Separator — the transmitted FNC1 inside data (ISO 18004 §7.4.9).
const GS: char = '\u{1d}';

/// `GenSpecs` figure 7.8.4-2: AIs whose first two digits predefine the TOTAL
/// element length (AI + value) — no GS separator needed after them.
const PREDEFINED_TOTAL_LEN: [(&str, usize); 20] = [
    ("00", 20),
    ("01", 16),
    ("02", 16),
    ("11", 8),
    ("12", 8),
    ("13", 8),
    ("14", 8),
    ("15", 8),
    ("16", 8),
    ("17", 8),
    ("18", 8),
    ("19", 8),
    ("20", 4),
    ("31", 10),
    ("32", 10),
    ("33", 10),
    ("34", 10),
    ("35", 10),
    ("36", 10),
    ("41", 16),
];

/// Value shape for a known AI.
#[derive(Clone, Copy)]
enum Shape {
    /// Exactly `n` digits.
    Digits(usize),
    /// Exactly `n` digits, last one a GS1 mod-10 check digit.
    DigitsCheck(usize),
    /// `YYMMDD` (DD=00 allowed: "end of month", `GenSpecs` 3.4.2).
    Date,
    /// 1..=n chars from CSET 82.
    Cset82(usize),
    /// 1..=n digits.
    VarDigits(usize),
}

/// AI → value shape for the validated subset (`GenSpecs` release 25 / ref.gs1.org/ai).
/// 4-digit metric AIs (31nn-36nn) are matched on their 3-digit stem.
fn shape_of(ai: &str) -> Option<Shape> {
    Some(match ai {
        "00" => Shape::DigitsCheck(18),
        "01" | "02" => Shape::DigitsCheck(14),
        "11" | "12" | "13" | "15" | "16" | "17" => Shape::Date,
        "20" => Shape::Digits(2),
        "10" | "21" | "22" => Shape::Cset82(20),
        "30" | "37" => Shape::VarDigits(8),
        "235" => Shape::Cset82(28),
        "8005" => Shape::Digits(6),
        "8011" => Shape::VarDigits(12),
        "8019" => Shape::VarDigits(10),
        "8020" => Shape::Cset82(25),
        "8200" => Shape::Cset82(70),
        "90" | "240" | "241" | "250" | "251" => Shape::Cset82(30),
        "91" | "92" | "93" | "94" | "95" | "96" | "97" | "98" | "99" => Shape::Cset82(90),
        _ if ai.len() == 3 && ("410".."418").contains(&ai) => Shape::DigitsCheck(13),
        _ if ai.len() == 4
            && matches!(&ai[..2], "31" | "32" | "33" | "34" | "35" | "36")
            && ai.as_bytes()[3].is_ascii_digit() =>
        {
            Shape::Digits(6)
        }
        _ => return None,
    })
}

/// GS1 mod-10 check digit (`GenSpecs` §7.2.7): weights 3,1,3,… from the digit
/// left of the check position.
pub(crate) fn check_digit_ok(digits: &str) -> bool {
    let bytes = digits.as_bytes();
    if bytes.is_empty() || !bytes.iter().all(u8::is_ascii_digit) {
        return false;
    }
    let mut sum = 0u32;
    // all positions except the last, weighted 3/1 alternating from the right
    for (i, b) in bytes[..bytes.len() - 1].iter().rev().enumerate() {
        let d = u32::from(b - b'0');
        sum += if i % 2 == 0 { d * 3 } else { d };
    }
    let expect = (10 - sum % 10) % 10;
    u32::from(bytes[bytes.len() - 1] - b'0') == expect
}

/// CSET 82 (`GenSpecs` figure 7.11-1) — GS1's own value-charset class.
fn is_cset82(c: char) -> bool {
    matches!(c,
        '0'..='9' | 'A'..='Z' | 'a'..='z'
        | '!' | '"' | '%' | '&' | '\'' | '(' | ')' | '*' | '+' | ','
        | '-' | '.' | '/' | ':' | ';' | '<' | '=' | '>' | '?' | '_')
}

fn valid_date(v: &str) -> bool {
    if v.len() != 6 || !v.bytes().all(|b| b.is_ascii_digit()) {
        return false;
    }
    let mm: u8 = v[2..4].parse().unwrap_or(99);
    let dd: u8 = v[4..6].parse().unwrap_or(99);
    (1..=12).contains(&mm) && dd <= 31
}

fn validate(ai: &str, value: &str, shape: Shape, issues: &mut Vec<String>) {
    let ok = match shape {
        Shape::Digits(n) => value.len() == n && value.bytes().all(|b| b.is_ascii_digit()),
        Shape::DigitsCheck(n) => {
            if value.len() == n && value.bytes().all(|b| b.is_ascii_digit()) {
                if check_digit_ok(value) {
                    true
                } else {
                    issues.push(format!(
                        "AI {ai}: check digit invalid for '{value}' (GenSpecs 7.2.7)"
                    ));
                    return;
                }
            } else {
                false
            }
        }
        Shape::Date => {
            if valid_date(value) {
                true
            } else {
                issues.push(format!(
                    "AI {ai}: invalid YYMMDD date '{value}' (GenSpecs 3.4)"
                ));
                return;
            }
        }
        Shape::Cset82(max) => (1..=max).contains(&value.len()) && value.chars().all(is_cset82),
        Shape::VarDigits(max) => {
            (1..=max).contains(&value.len()) && value.bytes().all(|b| b.is_ascii_digit())
        }
    };
    if !ok {
        issues.push(format!(
            "AI {ai}: value '{value}' violates the AI format (ref.gs1.org/ai/{ai})"
        ));
    }
}

/// Split at most `max_bytes` off the head WITHOUT crossing a char boundary
/// (FNC1 payloads are attacker-controlled UTF-8 — byte-index slicing
/// panicked on multi-byte content pre-fix; this is the one safe primitive
/// every byte-budget split in this module goes through).
fn split_head(s: &str, max_bytes: usize) -> (&str, &str) {
    let mut cut = max_bytes.min(s.len());
    while cut > 0 && !s.is_char_boundary(cut) {
        cut -= 1;
    }
    s.split_at(cut)
}

/// Longest known AI at the head of `s` (4 → 3 → 2 digits).
fn known_ai_at(s: &str) -> Option<&str> {
    for len in [4usize, 3, 2] {
        if let Some(head) = s.get(..len)
            && head.bytes().all(|b| b.is_ascii_digit())
            && shape_of(head).is_some()
        {
            return Some(head);
        }
    }
    None
}

/// Parse a GS1 element string (decoded FNC1-first QR data: AIs concatenated,
/// variable-length values terminated by GS). Never fails — problems land in
/// `issues`, each naming its criterion.
pub(crate) fn parse_element_string(data: &str) -> Gs1Data {
    let mut out = Gs1Data::default();
    let mut rest = data;
    while !rest.is_empty() {
        // a stray separator before the next AI is legal noise after a
        // predefined-length element; skip it
        if let Some(stripped) = rest.strip_prefix(GS) {
            rest = stripped;
            continue;
        }
        let Some(ai) = known_ai_at(rest) else {
            let fragment: String = rest.chars().take(8).collect();
            out.issues.push(format!(
                "unknown AI at '{fragment}…': not in the validated GenSpecs subset"
            ));
            // resync at the next GS — the rest of THIS element can't be split
            match rest.find(GS) {
                Some(i) => rest = &rest[i + 1..],
                None => break,
            }
            continue;
        };
        let ai = ai.to_owned();
        rest = &rest[ai.len()..];

        let predefined = PREDEFINED_TOTAL_LEN
            .iter()
            .find(|(p, _)| *p == &ai[..2])
            .map(|(_, total)| *total);
        let value = match predefined {
            Some(total) => {
                // boundary-safe split: a multi-byte char inside the value
                // zone shortens the cut — the value then fails shape
                // validation (CSET 82 / digits are ASCII-only), never panics
                let (v, tail) = split_head(rest, total - ai.len());
                rest = tail;
                if v.len() + ai.len() < total {
                    out.issues.push(format!(
                        "AI {ai}: truncated — predefined length is {total} incl. AI (GenSpecs 7.8.4)"
                    ));
                }
                v.to_owned()
            }
            None => match rest.find(GS) {
                Some(i) => {
                    let v = &rest[..i];
                    rest = &rest[i + 1..];
                    v.to_owned()
                }
                None => std::mem::take(&mut rest).to_owned(),
            },
        };

        if let Some(shape) = shape_of(&ai) {
            validate(&ai, &value, shape, &mut out.issues);
        }
        if ai == "01" && out.gtin.is_none() {
            out.gtin = Some(value.clone());
        }
        out.elements.push(Gs1Element { ai, value });
    }
    if out.elements.is_empty() {
        out.issues
            .push("no parseable GS1 element string (ISO 18004 §7.4.9 FNC1 data)".to_owned());
    }
    out
}

/// Primary keys we recognize in a Digital Link path: AI → value length.
const DL_PRIMARY_KEYS: [(&str, usize); 3] = [("01", 14), ("00", 18), ("414", 13)];
/// GTIN key qualifiers in their ONLY legal order (DL URI Syntax §4.4 ABNF).
const DL_QUALIFIER_ORDER: [&str; 3] = ["22", "10", "21"];

/// Recognize + validate a GS1 Digital Link URI (any domain — `id.gs1.org` is
/// just the canonical resolver). `None` = not Digital-Link-shaped at all;
/// `Some` with issues = DL-shaped but non-conformant.
pub(crate) fn parse_digital_link(url: &str) -> Option<Gs1Data> {
    let after_scheme = url
        .get(..8)
        .filter(|h| h.eq_ignore_ascii_case("https://"))
        .map(|_| &url[8..])
        .or_else(|| {
            url.get(..7)
                .filter(|h| h.eq_ignore_ascii_case("http://"))
                .map(|_| &url[7..])
        })?;
    let (_host, path_query) = after_scheme.split_once('/')?;
    let (path, query) = match path_query.split_once('?') {
        Some((p, q)) => (p, Some(q)),
        None => (path_query, None),
    };
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();

    // locate a primary key whose NEXT segment has the exact key length —
    // the shape guard that keeps /2024/01/15/ blog paths out
    let (key_idx, key_ai, key_len) = segments.iter().enumerate().find_map(|(i, seg)| {
        DL_PRIMARY_KEYS.iter().find_map(|(ai, len)| {
            (seg == ai
                && segments
                    .get(i + 1)
                    .is_some_and(|v| v.len() == *len && v.bytes().all(|b| b.is_ascii_digit())))
            .then_some((i, *ai, *len))
        })
    })?;

    let mut out = Gs1Data::default();
    let key_value = segments[key_idx + 1];
    debug_assert_eq!(key_value.len(), key_len);
    if !check_digit_ok(key_value) {
        out.issues.push(format!(
            "AI {key_ai}: check digit invalid for '{key_value}' (GenSpecs 7.2.7)"
        ));
    }
    if key_ai == "01" {
        out.gtin = Some(key_value.to_owned());
    }
    out.elements.push(Gs1Element {
        ai: key_ai.to_owned(),
        value: key_value.to_owned(),
    });

    // qualifiers after the key: /22/../10/../21/.. — order is normative
    let mut cursor = key_idx + 2;
    let mut min_rank = 0usize;
    while cursor < segments.len() {
        let ai_seg = segments[cursor];
        let Some(&value_seg) = segments.get(cursor + 1) else {
            out.issues.push(format!(
                "qualifier AI {ai_seg} has no value segment (DL URI Syntax 4.4)"
            ));
            break;
        };
        let Some(rank) = DL_QUALIFIER_ORDER.iter().position(|q| *q == ai_seg) else {
            out.issues.push(format!(
                "unexpected path segment '{ai_seg}' after the primary key (DL URI Syntax 4.4)"
            ));
            break;
        };
        if rank < min_rank {
            out.issues.push(format!(
                "qualifier AI {ai_seg} out of order — path order is 01→22→10→21 (DL URI Syntax 4.4)"
            ));
        }
        min_rank = min_rank.max(rank);
        if let Some(shape) = shape_of(ai_seg) {
            validate(ai_seg, value_seg, shape, &mut out.issues);
        }
        out.elements.push(Gs1Element {
            ai: ai_seg.to_owned(),
            value: value_seg.to_owned(),
        });
        cursor += 2;
    }

    // all-numeric query keys are AI data attributes (DL URI Syntax 4.5)
    if let Some(query) = query {
        for pair in query.split('&') {
            let Some((k, v)) = pair.split_once('=') else {
                continue;
            };
            if k.is_empty() || !k.bytes().all(|b| b.is_ascii_digit()) {
                continue;
            }
            if let Some(shape) = shape_of(k) {
                validate(k, v, shape, &mut out.issues);
            } else {
                out.issues.push(format!(
                    "unknown AI {k} in query (not in the validated subset)"
                ));
            }
            out.elements.push(Gs1Element {
                ai: k.to_owned(),
                value: v.to_owned(),
            });
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    const GS_S: &str = "\u{1d}";

    #[test]
    fn element_string_fixed_then_variable_then_fixed_no_separator_needed() {
        // 01 (predefined 16) + 10 batch (GS) + 17 expiry (predefined 8)
        let data = format!("010950600013435210ABC123{GS_S}17261231");
        let parsed = parse_element_string(&data);
        assert_eq!(parsed.issues, Vec::<String>::new());
        assert_eq!(parsed.gtin.as_deref(), Some("09506000134352"));
        assert_eq!(
            parsed.elements,
            vec![
                Gs1Element {
                    ai: "01".into(),
                    value: "09506000134352".into()
                },
                Gs1Element {
                    ai: "10".into(),
                    value: "ABC123".into()
                },
                Gs1Element {
                    ai: "17".into(),
                    value: "261231".into()
                },
            ]
        );
    }

    #[test]
    fn element_string_last_variable_needs_no_terminator() {
        let parsed = parse_element_string("010950600013435221SER42");
        assert!(parsed.issues.is_empty(), "{:?}", parsed.issues);
        assert_eq!(parsed.elements[1].ai, "21");
        assert_eq!(parsed.elements[1].value, "SER42");
    }

    #[test]
    fn element_string_metric_ai_and_sscc() {
        // 3103 = net weight kg, 3 decimals (predefined total 10) · 00 SSCC 18+check
        let parsed = parse_element_string("310300019500106141411234567897");
        assert!(parsed.issues.is_empty(), "{:?}", parsed.issues);
        assert_eq!(
            parsed.elements[0],
            Gs1Element {
                ai: "3103".into(),
                value: "000195".into()
            }
        );
        assert_eq!(parsed.elements[1].ai, "00");
    }

    #[test]
    fn element_string_bad_check_digit_names_the_criterion() {
        let parsed = parse_element_string("0109506000134353");
        assert_eq!(parsed.issues.len(), 1);
        assert!(
            parsed.issues[0].contains("GenSpecs 7.2.7"),
            "{:?}",
            parsed.issues
        );
        // gtin still extracted — consumers decide what to do with a bad one
        assert_eq!(parsed.gtin.as_deref(), Some("09506000134353"));
    }

    #[test]
    fn element_string_bad_date_and_cset_violation() {
        let parsed = parse_element_string(&format!("17269931{GS_S}10sp ace"));
        // 17269931 → date 269931 invalid (month 99)... wait: predefined 8 total
        assert!(
            parsed.issues.iter().any(|i| i.contains("GenSpecs 3.4")),
            "{:?}",
            parsed.issues
        );
        // space is outside CSET 82
        assert!(
            parsed
                .issues
                .iter()
                .any(|i| i.contains("ref.gs1.org/ai/10")),
            "{:?}",
            parsed.issues
        );
    }

    #[test]
    fn element_string_unknown_ai_flagged_and_resyncs_at_gs() {
        let parsed = parse_element_string(&format!("4399XYZ{GS_S}10OK"));
        assert!(
            parsed.issues.iter().any(|i| i.contains("unknown AI")),
            "{:?}",
            parsed.issues
        );
        assert!(
            parsed
                .elements
                .iter()
                .any(|e| e.ai == "10" && e.value == "OK"),
            "resync after GS must recover the next element: {:?}",
            parsed.elements
        );
    }

    #[test]
    fn element_string_multibyte_utf8_never_panics() {
        // attacker-controlled FNC1 payloads can carry arbitrary UTF-8 —
        // byte-index slicing must never cross a char boundary (live DoS
        // class: "01é" panicked at known_ai_at's s[..3] pre-fix)
        for hostile in [
            "01é",
            "é0109506000134352",
            "0109506000134352é10ABC\u{1d}é",
            "010950600013435é",
            "10\u{e9}\u{1d}21x",
            "🦋",
            "9é",
        ] {
            let parsed = parse_element_string(hostile);
            // non-conformant is fine; panicking is the bug
            assert!(
                !parsed.issues.is_empty() || !parsed.elements.is_empty(),
                "{hostile}"
            );
        }
    }

    proptest::proptest! {
        #[test]
        fn element_string_parser_never_panics(s in ".*") {
            let _ = parse_element_string(&s);
        }

        #[test]
        fn digital_link_parser_never_panics(s in ".*") {
            let _ = parse_digital_link(&s);
        }
    }

    #[test]
    fn check_digit_known_goods() {
        for good in ["09506000134352", "00614141123452", "106141411234567897"] {
            assert!(check_digit_ok(good), "{good}");
        }
        assert!(!check_digit_ok("09506000134353"));
        assert!(!check_digit_ok(""));
        assert!(!check_digit_ok("12a4"));
    }

    #[test]
    fn digital_link_canonical_example_is_conformant() {
        // the GS1 DL URI Syntax worked example
        let dl = parse_digital_link(
            "https://id.gs1.org/01/09506000134352/10/ABC123/21/456789?17=261231",
        )
        .expect("DL-shaped");
        assert_eq!(dl.issues, Vec::<String>::new());
        assert_eq!(dl.gtin.as_deref(), Some("09506000134352"));
        let ais: Vec<&str> = dl.elements.iter().map(|e| e.ai.as_str()).collect();
        assert_eq!(ais, vec!["01", "10", "21", "17"]);
    }

    #[test]
    fn digital_link_works_on_any_domain() {
        let dl = parse_digital_link("https://qrc-ai.com/01/09506000134352").expect("DL");
        assert!(dl.issues.is_empty());
        assert_eq!(dl.gtin.as_deref(), Some("09506000134352"));
    }

    #[test]
    fn digital_link_qualifier_order_violation_flagged() {
        let dl = parse_digital_link("https://id.gs1.org/01/09506000134352/21/456789/10/ABC123")
            .expect("DL-shaped");
        assert!(
            dl.issues.iter().any(|i| i.contains("01→22→10→21")),
            "{:?}",
            dl.issues
        );
    }

    #[test]
    fn digital_link_rejects_non_14_digit_gtin_and_short_names() {
        // 13-digit GTIN path → not even DL-shaped under v1.4 (14-digit law);
        // the shape guard refuses, the URL stays a plain URL
        assert!(parse_digital_link("https://id.gs1.org/01/9506000134352").is_none());
        // short names were REMOVED in DL 1.3 — never DL-shaped
        assert!(parse_digital_link("https://id.gs1.org/gtin/09506000134352").is_none());
    }

    #[test]
    fn digital_link_blog_path_false_positive_guard() {
        assert!(parse_digital_link("https://example.com/2024/01/15/post").is_none());
        assert!(parse_digital_link("https://example.com/").is_none());
        assert!(parse_digital_link("not a url").is_none());
    }

    #[test]
    fn digital_link_bad_query_attribute_flagged() {
        let dl = parse_digital_link("https://id.gs1.org/01/09506000134352?17=269931")
            .expect("DL-shaped");
        assert!(
            dl.issues.iter().any(|i| i.contains("GenSpecs 3.4")),
            "{:?}",
            dl.issues
        );
    }
}

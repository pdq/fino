use fino::text_length::{LengthError, LengthUnit, TextLength, compile};
use unicode_segmentation::UnicodeSegmentation;

fn exact(unit: LengthUnit, length: usize, text: &str) -> bool {
    compile(&TextLength {
        min: Some(length),
        max: Some(length),
        unit,
    })
    .unwrap()
    .matches(text)
}

#[test]
fn bytes_scalars_and_extended_graphemes_are_distinct() {
    for (text, bytes, scalars, graphemes) in [
        ("", 0, 0, 0),
        ("plain", 5, 5, 5),
        ("é", 2, 1, 1),
        ("a\u{301}", 3, 2, 1),
        ("👩\u{200d}🔬", 11, 3, 1),
        ("🇳🇿", 8, 2, 1),
        ("\u{301}\u{302}", 4, 2, 1),
    ] {
        assert!(exact(LengthUnit::Bytes, bytes, text));
        assert!(exact(LengthUnit::Scalars, scalars, text));
        assert!(exact(LengthUnit::Graphemes, graphemes, text));
    }
}

#[test]
fn bounds_are_inclusive_and_independently_optional() {
    for unit in [
        LengthUnit::Bytes,
        LengthUnit::Scalars,
        LengthUnit::Graphemes,
    ] {
        let bounded = compile(&TextLength {
            min: Some(2),
            max: Some(3),
            unit,
        })
        .unwrap();
        assert!(!bounded.matches("a"));
        assert!(bounded.matches("ab"));
        assert!(bounded.matches("abc"));
        assert!(!bounded.matches("abcd"));
        let minimum = compile(&TextLength {
            min: Some(2),
            max: None,
            unit,
        })
        .unwrap();
        assert!(!minimum.matches("a"));
        assert!(minimum.matches("ab"));
        assert!(minimum.matches("abcdefgh"));
        let maximum = compile(&TextLength {
            min: None,
            max: Some(2),
            unit,
        })
        .unwrap();
        assert!(maximum.matches(""));
        assert!(maximum.matches("ab"));
        assert!(!maximum.matches("abc"));
    }
}

#[test]
fn empty_and_inverted_bounds_fail_before_matching() {
    for unit in [
        LengthUnit::Bytes,
        LengthUnit::Scalars,
        LengthUnit::Graphemes,
    ] {
        assert_eq!(
            compile(&TextLength {
                min: None,
                max: None,
                unit
            })
            .unwrap_err(),
            LengthError::NoBounds
        );
        assert_eq!(
            compile(&TextLength {
                min: Some(4),
                max: Some(3),
                unit
            })
            .unwrap_err(),
            LengthError::InvertedBounds
        );
        assert!(
            compile(&TextLength {
                min: Some(0),
                max: None,
                unit
            })
            .unwrap()
            .matches("")
        );
        let empty = compile(&TextLength {
            min: None,
            max: Some(0),
            unit,
        })
        .unwrap();
        assert!(empty.matches(""));
        assert!(!empty.matches("x"));
    }
    assert!(!LengthError::NoBounds.to_string().is_empty());
    assert!(!LengthError::InvertedBounds.to_string().is_empty());
}

#[test]
fn extreme_bounds_do_not_overflow_or_require_large_allocations() {
    for unit in [
        LengthUnit::Bytes,
        LengthUnit::Scalars,
        LengthUnit::Graphemes,
    ] {
        let maximum = compile(&TextLength {
            min: None,
            max: Some(usize::MAX),
            unit,
        })
        .unwrap();
        assert!(maximum.matches("a\u{301}\r\n"));
        let minimum = compile(&TextLength {
            min: Some(usize::MAX),
            max: None,
            unit,
        })
        .unwrap();
        assert!(!minimum.matches("a\u{301}\r\n"));
    }
}

#[test]
fn line_endings_and_spacing_are_never_trimmed_or_split() {
    for (text, bytes, graphemes) in [
        ("\r\n", 2, 1),
        ("\n", 1, 1),
        ("\r", 1, 1),
        ("a\r\nb\nc\r", 7, 6),
        (" \t", 2, 2),
        ("\r\r\n\n", 4, 3),
    ] {
        assert!(exact(LengthUnit::Bytes, bytes, text));
        assert!(exact(LengthUnit::Scalars, bytes, text));
        assert!(exact(LengthUnit::Graphemes, graphemes, text));
    }
}

#[test]
fn ascii_fast_path_matches_uax29_including_all_controls_and_crlf_pairs() {
    for first in 0..=127u8 {
        for second in 0..=127u8 {
            let bytes = [first, second];
            let text = std::str::from_utf8(&bytes).unwrap();
            assert!(exact(
                LengthUnit::Graphemes,
                text.graphemes(true).count(),
                text
            ));
        }
    }
    let text = "abc\r\n\tdef\0\r\n".repeat(100);
    assert!(exact(
        LengthUnit::Graphemes,
        text.graphemes(true).count(),
        &text
    ));
}

#[test]
fn thresholds_match_repeated_combining_and_joined_sequences() {
    for cluster in ["a\u{301}", "👩\u{200d}🔬"] {
        let text = cluster.repeat(37);
        let graphemes = compile(&TextLength {
            min: Some(38),
            max: None,
            unit: LengthUnit::Graphemes,
        })
        .unwrap();
        let scalars = compile(&TextLength {
            min: Some(38),
            max: None,
            unit: LengthUnit::Scalars,
        })
        .unwrap();
        assert!(!graphemes.matches(&text));
        assert!(scalars.matches(&text));
    }
}

#[test]
fn serialization_defaults_to_scalars_and_rejects_unknown_or_invalid_data() {
    let spec: TextLength = serde_json::from_str(r#"{"min":1}"#).unwrap();
    assert_eq!(spec.unit, LengthUnit::Scalars);
    assert!(compile(&spec).unwrap().matches("x"));
    for invalid in [
        r#"{"min":-1}"#,
        r#"{"max":1.5}"#,
        r#"{"min":1,"unit":"columns"}"#,
        r#"{"min":1,"extra":true}"#,
    ] {
        assert!(serde_json::from_str::<TextLength>(invalid).is_err());
    }
    let spec: TextLength = serde_json::from_str(r#"{"max":2,"unit":"graphemes"}"#).unwrap();
    assert!(compile(&spec).unwrap().matches("a\u{301}"));
    let encoded = serde_json::to_value(spec).unwrap();
    assert_eq!(encoded["unit"], "graphemes");
    assert!(
        !serde_json::to_string(&schemars::schema_for!(TextLength))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn requested_units_preserve_width_and_normalization_independence() {
    // Both are one grapheme, despite different byte/scalar counts. A tab and a
    // wide symbol also count as one, independent of terminal display width.
    for text in ["é", "e\u{301}", "\t", "界"] {
        assert!(exact(LengthUnit::Graphemes, 1, text));
    }
}

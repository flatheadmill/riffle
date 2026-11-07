#[test]
fn test_ordinary_grammar() {
    for text in [
        "",
        "the",
        "the a",
        "a AND NOT b",
        "+a -b",
        "a OR b AND c",
        "(a OR b)^2",
        "title:(oak OR elm)",
        "title:oak^3",
        "title:\"oak elm\"~2",
        "title:\"oak elm\"*",
        "signed:[1 TO 10]",
        "signed:{1 TO 10}",
        "signed:[* TO 10]",
        "signed:IN [1 2]",
        "title:*",
        "title:/o(a|b)k/",
        "json.name:oak",
        "title:oak AND with_stop_words:the",
        "title:",
        "kind:park AND (open:)",
        "(the)^2",
        "title:oak -title:elm",
    ] {
        let expected = tantivy::query_grammar::parse_query(text);
        let actual = query_grammar::parse_query(text);
        assert_eq!(actual.is_ok(), expected.is_ok(), "{text}");
        if let (Ok(actual), Ok(expected)) = (actual, expected) {
            assert_eq!(
                serde_json::to_value(actual).unwrap(),
                serde_json::to_value(expected).unwrap(),
                "{text}"
            );
        }
        let actual = query_grammar::parse_query_lenient(text);
        let expected = tantivy::query_grammar::parse_query_lenient(text);
        assert_eq!(
            serde_json::to_value(actual).unwrap(),
            serde_json::to_value(expected).unwrap(),
            "{text}"
        );
    }
}

#[test]
fn test_boosted_duplicates_differ_from_tantivy() {
    let text = "(title:oak title:oak)^2";
    let actual = query_grammar::parse_query(text).unwrap();
    let query_grammar::UserInputAst::Boost(child, _) = &actual else {
        panic!("expected boost");
    };
    let query_grammar::UserInputAst::Clause(children) = child.as_ref() else {
        panic!("expected retained group");
    };
    assert_eq!(children.len(), 1);
    assert_eq!(
        children[0].1,
        query_grammar::parse_query("title:oak").unwrap()
    );
    let upstream = tantivy::query_grammar::parse_query(text).unwrap();
    assert_ne!(
        serde_json::to_value(actual).unwrap(),
        serde_json::to_value(upstream).unwrap()
    );
}

#[test]
fn test_spatial_calls_differ_from_tantivy() {
    let valid = "geo:$intersects(0 0, 1 0, 0 1)";
    let actual = query_grammar::parse_query(valid).unwrap();
    assert!(matches!(actual, query_grammar::UserInputAst::Leaf(ref leaf)
        if matches!(leaf.as_ref(), query_grammar::UserInputLeaf::Spatial { .. })));
    for text in [valid, "geo:$intersects(1 2)"] {
        let actual = query_grammar::parse_query_lenient(text);
        let upstream = tantivy::query_grammar::parse_query_lenient(text);
        assert_ne!(
            serde_json::to_value(actual).unwrap(),
            serde_json::to_value(upstream).unwrap(),
            "{text}"
        );
    }
    assert!(query_grammar::parse_query("geo:$intersects(1 2)").is_err());
}

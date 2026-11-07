use super::*;
use tantivy::collector::{Count, TopDocs};
use tantivy::schema::{IndexRecordOption, TextFieldIndexing, TextOptions, INDEXED, TEXT};
use tantivy::tokenizer::{LowerCaser, SimpleTokenizer, StopWordFilter, TextAnalyzer};
use tantivy::{doc, DocAddress, Searcher};

struct Fixture {
    index: Index,
    searcher: Searcher,
    parser: QueryParser,
    ordinary: tantivy::query::QueryParser,
}

impl Fixture {
    fn new() -> tantivy::Result<Self> {
        let mut schema = Schema::builder();
        let title = schema.add_text_field("title", TEXT);
        let body = schema.add_text_field("body", TEXT);
        let number = schema.add_i64_field("number", INDEXED);
        schema.add_text_field(
            "stop",
            TextOptions::default().set_indexing_options(
                TextFieldIndexing::default()
                    .set_tokenizer("stop")
                    .set_index_option(IndexRecordOption::WithFreqsAndPositions),
            ),
        );
        let index = Index::create_in_ram(schema.build());
        index.tokenizers().register(
            "stop",
            TextAnalyzer::builder(SimpleTokenizer::default())
                .filter(LowerCaser)
                .filter(StopWordFilter::remove(vec!["the".into(), "a".into()]))
                .build(),
        );
        let mut writer = index.writer_with_num_threads(1, 15_000_000)?;
        for (title_value, body_value, value) in [
            ("oak elm", "oak", 5i64),
            ("elm", "oak elm", 20),
            ("pine", "ash", 40),
        ] {
            writer.add_document(doc!(title => title_value, body => body_value, number => value))?;
        }
        writer.commit()?;
        let searcher = index.reader()?.searcher();
        let mut parser = QueryParser::for_index(&index, vec![title, body]);
        let mut ordinary = tantivy::query::QueryParser::for_index(&index, vec![title, body]);
        parser.set_field_boost(title, 2.0);
        ordinary.set_field_boost(title, 2.0);
        parser.set_field_fuzzy(body, false, 1, true);
        ordinary.set_field_fuzzy(body, false, 1, true);
        parser.allow_regexes();
        ordinary.allow_regexes();
        Ok(Self {
            index,
            searcher,
            parser,
            ordinary,
        })
    }

    fn scores(&self, query: &dyn Query) -> tantivy::Result<Vec<(Score, DocAddress)>> {
        let mut scores = self
            .searcher
            .search(query, &TopDocs::with_limit(10).order_by_score())?;
        scores.sort_by_key(|(_, address)| *address);
        Ok(scores)
    }

    fn same(&self, actual: &dyn Query, expected: &dyn Query) -> tantivy::Result<()> {
        let actual = self.scores(actual)?;
        let expected = self.scores(expected)?;
        assert_eq!(actual.len(), expected.len());
        for ((a, address_a), (b, address_b)) in actual.into_iter().zip(expected) {
            assert_eq!(address_a, address_b);
            assert!((a - b).abs() < 1e-5, "{a} != {b}");
        }
        Ok(())
    }
}

#[test]
fn test_leaf_omission_and_partial_recovery() -> tantivy::Result<()> {
    let fixture = Fixture::new()?;
    let oak = fixture.parser.parse_query("title:oak")?;
    for (text, omitted, error_count) in [
        ("stop:the", true, 0),
        ("number:[* TO *]", true, 0),
        ("missing:oak", true, 1),
        ("number:IN [bad]", false, 1),
        ("number:[bad TO 10]", false, 1),
    ] {
        let UserInputAst::Leaf(leaf) = query_grammar::parse_query(text).unwrap() else {
            panic!("expected leaf");
        };
        let (node, errors) = fixture.parser.resolve_leaf(*leaf);
        assert_eq!(matches!(node, Node::Omitted), omitted, "{text}");
        assert_eq!(errors.len(), error_count, "{text}");
        let (query, errors) = fixture
            .parser
            .parse_query_lenient(&format!("title:oak AND {text}"));
        assert_eq!(errors.len(), error_count);
        if text == "number:IN [bad]" {
            assert_eq!(fixture.searcher.search(query.as_ref(), &Count)?, 0);
        } else {
            // The partly recovered range still selects the first document.
            assert_eq!(fixture.searcher.search(query.as_ref(), &Count)?, 1);
            if omitted {
                fixture.same(query.as_ref(), oak.as_ref())?;
            }
        }
        assert_eq!(fixture.parser.parse_query(text).is_err(), error_count != 0);
    }
    let empty_set = UserInputLeaf::Set {
        field: Some("number".into()),
        elements: Vec::new(),
    };
    let (node, errors) = fixture.parser.resolve_leaf(empty_set);
    assert!(matches!(node, Node::Query(_)));
    assert!(errors.is_empty());
    assert_eq!(fixture.searcher.search(node.compile().as_ref(), &Count)?, 0);

    let schema = fixture.index.schema();
    let parser = QueryParser::for_index(
        &fixture.index,
        vec![schema.get_field("number")?, schema.get_field("title")?],
    );
    assert!(parser.parse_query("oak").is_ok());
    assert!(parser.parse_query_lenient("oak").1.is_empty());
    Ok(())
}

#[test]
fn test_ordinary_resolution_matches_tantivy() -> tantivy::Result<()> {
    let mut fixture = Fixture::new()?;
    for conjunction in [false, true] {
        if conjunction {
            fixture.parser.set_conjunction_by_default();
            fixture.ordinary.set_conjunction_by_default();
        }
        for text in [
            "oak elm",
            "title:oak OR body:elm",
            "+title:elm body:oak",
            "title:oak -body:pine",
            "title:\"oak elm\"",
            "title:\"oak elm\"*",
            "title:\"oak elm\"~2",
            "title:/o.*/",
            "number:[1 TO 10]",
            "number:IN [5 40]",
            "title:(oak OR pine)",
            "(title:oak OR title:pine)^3",
            "title:oak AND stop:the",
            "title:oak AND stop:the^2",
            "stop:the",
        ] {
            let expected = fixture.ordinary.parse_query(text)?;
            let ast = query_grammar::parse_query(text).unwrap();
            let (lenient, errors) = fixture.parser.parse_query_lenient(text);
            assert!(errors.is_empty(), "{text}: {errors:?}");
            let (lenient_ast, errors) = fixture
                .parser
                .build_query_from_user_input_ast_lenient(ast.clone());
            assert!(errors.is_empty(), "{text}: {errors:?}");
            for actual in [
                fixture.parser.parse_query(text)?,
                fixture.parser.build_query_from_user_input_ast(ast)?,
                lenient,
                lenient_ast,
            ] {
                fixture.same(actual.as_ref(), expected.as_ref())?;
            }
        }
    }
    for text in [
        "number:[bad TO 10]",
        "number:IN [bad]",
        "missing:oak",
        "stop:the OR missing:oak",
    ] {
        let (actual, errors) = fixture.parser.parse_query_lenient(text);
        let (expected, expected_errors) = fixture.ordinary.parse_query_lenient(text);
        assert_eq!(errors, expected_errors);
        fixture.same(actual.as_ref(), expected.as_ref())?;
        assert_eq!(
            fixture.parser.parse_query(text).unwrap_err(),
            fixture.ordinary.parse_query(text).unwrap_err()
        );
    }
    Ok(())
}

#[test]
fn test_empty_operands_and_nested_complements() -> tantivy::Result<()> {
    let fixture = Fixture::new()?;
    let oak_ast = query_grammar::parse_query("title:oak").unwrap();
    let oak = fixture.parser.parse_query("title:oak")?;
    for ast in [
        UserInputAst::and(vec![oak_ast.clone(), UserInputAst::empty_query()]),
        UserInputAst::and(vec![UserInputAst::empty_query(), oak_ast.clone()]),
    ] {
        let query = fixture.parser.build_query_from_user_input_ast(ast)?;
        assert_eq!(fixture.searcher.search(query.as_ref(), &Count)?, 0);
    }
    let query = fixture
        .parser
        .build_query_from_user_input_ast(UserInputAst::or(vec![
            oak_ast,
            UserInputAst::empty_query(),
        ]))?;
    fixture.same(query.as_ref(), oak.as_ref())?;
    for text in [
        "title:oak AND NOT title:pine",
        "title:oak AND (NOT title:pine)^3",
        "title:oak AND NOT NOT title:oak",
        "title:oak AND (stop:the -title:pine)",
    ] {
        let query = fixture.parser.parse_query(text)?;
        fixture.same(query.as_ref(), oak.as_ref())?;
    }
    assert!(matches!(
        fixture.parser.parse_query("NOT title:pine"),
        Err(QueryParserError::AllButQueryForbidden)
    ));
    let (query, errors) = fixture.parser.parse_query_lenient("(stop:the)^2");
    assert_eq!(errors, vec![QueryParserError::AllButQueryForbidden]);
    let scores = fixture.scores(query.as_ref())?;
    assert_eq!(scores.len(), 3);
    assert!(scores.iter().all(|(score, _)| *score == 2.0));
    assert!(matches!(
        fixture.parser.parse_query("stop:the stop:a"),
        Err(QueryParserError::AllButQueryForbidden)
    ));
    assert!(matches!(
        fixture.parser.parse_query("stop:the -title:pine"),
        Err(QueryParserError::AllButQueryForbidden)
    ));
    let pine = query_grammar::parse_query("title:pine").unwrap();
    let direct = UserInputAst::Clause(vec![
        (Some(query_grammar::Occur::MustNot), pine.clone()),
        (
            Some(query_grammar::Occur::Should),
            UserInputAst::empty_query(),
        ),
    ]);
    for ast in [
        direct.clone(),
        UserInputAst::and(vec![
            query_grammar::parse_query("title:oak").unwrap(),
            direct,
        ]),
    ] {
        let query = fixture.parser.build_query_from_user_input_ast(ast)?;
        assert_eq!(fixture.searcher.search(query.as_ref(), &Count)?, 0);
    }
    let grouped = UserInputAst::or(vec![
        pine.unary(query_grammar::Occur::MustNot),
        UserInputAst::empty_query(),
    ]);
    let query = fixture.parser.build_query_from_user_input_ast(grouped)?;
    let scores = fixture.scores(query.as_ref())?;
    assert_eq!(scores.len(), 2);
    assert!(scores.iter().all(|(score, _)| *score == 0.0));
    Ok(())
}

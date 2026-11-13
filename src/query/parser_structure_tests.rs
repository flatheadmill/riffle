use tantivy::schema::{IndexRecordOption, TEXT};
use tantivy::Term;

use super::*;

fn make_query_parser() -> QueryParser {
    let mut schema = Schema::builder();
    let title = schema.add_text_field("title", TEXT);
    schema.add_custom_field(
        "geometry",
        "spatial",
        serde_json::json!({"index": "sphere"}),
    );
    QueryParser::new(schema.build(), vec![title], TokenizerManager::default())
}

#[test]
fn test_parser_keeps_supported_literal_spatial_forms() {
    let parser = make_query_parser();
    for text in [
        "geometry:$intersects(0 0, 1 0, 0 1)",
        "geometry:$contains(0 0, 1 0, 0 1)",
        "geometry:$covers(0 0, 1 0, 0 1)",
        "geometry:$coveredby(0 0, 1 0, 0 1)",
        "geometry:$within(1km, 0 0)",
    ] {
        let ast = query_grammar::parse_query(text).unwrap();
        assert!(parser
            .parse_query(text)
            .unwrap()
            .is::<crate::query::SpatialQuery>());
        assert!(parser
            .build_query_from_user_input_ast(ast.clone())
            .unwrap()
            .is::<crate::query::SpatialQuery>());
        let (query, errors) = parser.parse_query_lenient(text);
        assert!(errors.is_empty(), "{errors:?}");
        assert!(query.is::<crate::query::SpatialQuery>());
        let (query, errors) = parser.build_query_from_user_input_ast_lenient(ast);
        assert!(errors.is_empty(), "{errors:?}");
        assert!(query.is::<crate::query::SpatialQuery>());
    }
}

fn boolean_child(query: &dyn Query, index: usize) -> &dyn Query {
    query
        .downcast_ref::<tantivy::query::BooleanQuery>()
        .unwrap()
        .clauses()[index]
        .1
        .as_ref()
}

fn join_inputs(query: &dyn Query) -> (&dyn Query, &dyn Query) {
    use crate::query::{PlanNode, SpatialExecutor};

    let executor = query.downcast_ref::<SpatialExecutor>().unwrap();
    let PlanNode::Join { outer, inner, .. } = executor.root() else {
        panic!("expected a join");
    };
    let (PlanNode::Query(outer), PlanNode::Query(inner)) = (outer.as_ref(), inner.as_ref()) else {
        panic!("expected query inputs");
    };
    (outer.as_ref(), inner.as_ref())
}

fn assert_join_filters(parser: &QueryParser, query: &dyn Query, filters: &[&str]) {
    use tantivy::query::{AllQuery, BooleanQuery};

    let expected: Box<dyn Query> = if filters.is_empty() {
        Box::new(AllQuery)
    } else {
        Box::new(BooleanQuery::intersection(
            filters
                .iter()
                .map(|text| parser.parse_query(text).unwrap())
                .collect(),
        ))
    };
    let (outer, _) = join_inputs(query);
    assert_eq!(format!("{outer:?}"), format!("{expected:?}"));
}

#[test]
fn test_spatial_filters_follow_required_clauses() {
    let parser = make_query_parser();
    let join = "geometry:$within(1km, $query(title:c))";
    for (text, path, filters) in [
        (
            format!("title:a AND (title:b AND {join})"),
            vec![1, 1],
            vec!["title:a", "title:b"],
        ),
        (
            format!("title:a AND (title:b OR {join})"),
            vec![1, 1],
            vec!["title:a"],
        ),
        (format!("title:b OR {join}"), vec![1], vec![]),
        (format!("+title:a {join}"), vec![1], vec!["title:a"]),
        (format!("+title:a -{join}"), vec![1], vec!["title:a"]),
        (format!("-title:b +{join}"), vec![1], vec![]),
        (
            format!("+title:a -title:b +{join}"),
            vec![2],
            vec!["title:a"],
        ),
        (
            format!("(title:a OR title:b) AND {join}"),
            vec![1],
            vec!["title:a OR title:b"],
        ),
        (
            format!("(title:a OR title:b)^2 AND {join}"),
            vec![1],
            vec!["(title:a OR title:b)^2"],
        ),
        (
            format!("geometry:$intersects(0 0, 1 0, 0 1) AND {join}"),
            vec![1],
            vec!["geometry:$intersects(0 0, 1 0, 0 1)"],
        ),
    ] {
        let ast = query_grammar::parse_query(&text).unwrap();
        let query = parser.build_query_from_user_input_ast(ast).unwrap();
        let mut child = query.as_ref();
        for index in path {
            child = boolean_child(child, index);
        }
        assert_join_filters(&parser, child, &filters);
    }
}

#[test]
fn test_spatial_filters_keep_required_negation_groups() {
    use tantivy::query::{AllQuery, BooleanQuery, ConstScoreQuery, Occur};

    let parser = make_query_parser();
    let query = parser
        .parse_query("NOT title:a AND geometry:$within(1km, $query(title:b))")
        .unwrap();
    let excluded = BooleanQuery::new(vec![
        (Occur::MustNot, parser.parse_query("title:a").unwrap()),
        (
            Occur::Must,
            Box::new(ConstScoreQuery::new(Box::new(AllQuery), 0.0)),
        ),
    ]);
    let expected = BooleanQuery::intersection(vec![Box::new(excluded)]);
    let (outer, _) = join_inputs(boolean_child(query.as_ref(), 1));
    assert_eq!(format!("{outer:?}"), format!("{expected:?}"));
}

#[test]
fn test_spatial_filters_stay_in_or_branches() {
    let parser = make_query_parser();
    let query = parser
        .parse_query(
            "(title:a AND geometry:$within(1km, $query(title:c))) OR (title:b AND \
             geometry:$within(1km, $query(title:d)))",
        )
        .unwrap();
    for (index, filter) in ["title:a", "title:b"].into_iter().enumerate() {
        let branch = boolean_child(query.as_ref(), index);
        assert_join_filters(&parser, boolean_child(branch, 1), &[filter]);
    }
}

#[test]
fn test_spatial_filters_exclude_join_subtrees() {
    let parser = make_query_parser();
    let first = "geometry:$within(1km, $query(title:b))";
    let second = "geometry:$within(1km, $query(title:c))";
    for text in [
        format!("title:a AND {first} AND {second}"),
        format!("title:a AND ({first})^2 AND {second}"),
        format!("title:a AND (title:d OR {first}) AND {second}"),
    ] {
        let query = parser.parse_query(&text).unwrap();
        assert_join_filters(&parser, boolean_child(query.as_ref(), 2), &["title:a"]);
    }
}

#[test]
fn test_spatial_inner_filters_start_at_each_input() {
    let parser = make_query_parser();
    let query = parser
        .parse_query(
            "title:a AND geometry:$within(1km, $query(title:b AND geometry:$within(1km, \
             $query(title:c))))",
        )
        .unwrap();
    let outer_join = boolean_child(query.as_ref(), 1);
    assert_join_filters(&parser, outer_join, &["title:a"]);
    let (_, inner) = join_inputs(outer_join);
    let inner_join = boolean_child(inner, 1);
    assert_join_filters(&parser, inner_join, &["title:b"]);
    let (_, inner) = join_inputs(inner_join);
    assert_eq!(
        format!("{inner:?}"),
        format!("{:?}", parser.parse_query("title:c").unwrap())
    );
}

#[test]
fn test_ordinary_conversion_preserves_occurrences() {
    use tantivy::query::{BooleanQuery, BoostQuery, Occur, TermQuery};

    let parser = make_query_parser();
    let title = parser.schema.get_field("title").unwrap();
    for (text, occurs, minimum, boost) in [
        ("title:a title:b", [Occur::Should, Occur::Should], 1, 1.0),
        ("+title:a title:b", [Occur::Must, Occur::Should], 0, 1.0),
        ("title:a AND title:b", [Occur::Must, Occur::Must], 0, 1.0),
        ("title:a OR title:b", [Occur::Should, Occur::Should], 1, 1.0),
        (
            "(title:a title:b)^2",
            [Occur::Should, Occur::Should],
            1,
            2.0,
        ),
    ] {
        let expected = BooleanQuery::with_minimum_required_clauses(
            occurs
                .into_iter()
                .zip(["a", "b"])
                .map(|(occur, word)| {
                    (
                        occur,
                        Box::new(TermQuery::new(
                            Term::from_field_text(title, word),
                            IndexRecordOption::WithFreqs,
                        )) as Box<dyn Query>,
                    )
                })
                .collect(),
            minimum,
        );
        let expected: Box<dyn Query> = if boost == 1.0 {
            Box::new(expected)
        } else {
            Box::new(BoostQuery::new(Box::new(expected), boost))
        };
        let ast = query_grammar::parse_query(text).unwrap();
        for query in [
            parser.parse_query(text).unwrap(),
            parser.build_query_from_user_input_ast(ast).unwrap(),
        ] {
            assert_eq!(format!("{query:?}"), format!("{expected:?}"), "{text}");
        }
    }
}

#[test]
fn test_spatial_inputs_use_parser_settings() {
    use tantivy::schema::{TextFieldIndexing, TextOptions};
    use tantivy::tokenizer::{LowerCaser, SimpleTokenizer, StopWordFilter, TextAnalyzer};
    let mut schema = Schema::builder();
    let title = schema.add_text_field("title", TEXT);
    let body = schema.add_text_field("body", TEXT);
    schema.add_text_field(
        "stop",
        TextOptions::default().set_indexing_options(
            TextFieldIndexing::default()
                .set_tokenizer("stop")
                .set_index_option(IndexRecordOption::WithFreqsAndPositions),
        ),
    );
    schema.add_custom_field(
        "geometry",
        "spatial",
        serde_json::json!({"index": "sphere"}),
    );
    let schema = schema.build();
    let tokenizers = TokenizerManager::default();
    tokenizers.register(
        "stop",
        TextAnalyzer::builder(SimpleTokenizer::default())
            .filter(LowerCaser)
            .filter(StopWordFilter::remove(vec!["the".into()]))
            .build(),
    );
    for defaults in [vec![title, body], vec![body]] {
        for conjunction in [false, true] {
            let mut parser = QueryParser::new(schema.clone(), defaults.clone(), tokenizers.clone());
            parser.set_field_boost(title, 3.0);
            parser.set_field_fuzzy(body, false, 1, true);
            if conjunction {
                parser.set_conjunction_by_default();
            }
            for regexes in [false, true] {
                if regexes {
                    parser.allow_regexes();
                }
                for text in [
                    "Oak",
                    "Oak birch",
                    "title:Oak",
                    "body:oal",
                    "stop:\"THE Oak\"",
                    "title:/o.*/",
                    "title:oak AND unknown:x",
                    "NOT title:elm",
                ] {
                    let (query, errors) = parser
                        .parse_query_lenient(&format!("geometry:$within(1km, $query({text}))"));
                    let (_, inner) = join_inputs(query.as_ref());
                    let (standalone, expected_errors) = parser.parse_query_lenient(text);
                    assert_eq!(errors, expected_errors, "{text}");
                    assert_eq!(format!("{inner:?}"), format!("{standalone:?}"), "{text}");
                }
            }
        }
    }
}

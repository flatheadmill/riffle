use super::*;
use crate::test_support::parse_document;
use serde_json::json;
use tantivy::collector::Count;
use tantivy::merge_policy::NoMergePolicy;
use tantivy::query::TermQuery;
use tantivy::schema::{IndexRecordOption, Schema, STRING};
use tantivy::{IndexWriter, Term};

#[test]
fn test_multi_value_join_survives_deletion_and_merge() -> tantivy::Result<()> {
    use crate::query::QueryParser;

    let mut builder = Schema::builder();
    builder.add_custom_field("geometry", "spatial", json!({"index": "plane"}));
    let kind = builder.add_text_field("kind", STRING);
    let schema = builder.build();
    let index = crate::create_index_in_ram(schema.clone())?;
    let mut writer: IndexWriter = index.writer_with_num_threads(1, 50_000_000)?;
    writer.set_merge_policy(Box::new(NoMergePolicy));
    let batches: &[&[&str]] = &[
        &[
            r#"{"kind":"outer","geometry":[{"type":"Polygon","coordinates":[]},{"type":"Point","coordinates":[50,50]},{"type":"Polygon","coordinates":[[[0,0],[2,0],[2,2],[0,2],[0,0]]]}]}"#,
            r#"{"kind":"gone","geometry":{"type":"Polygon","coordinates":[[[0,0],[2,0],[2,2],[0,2],[0,0]]]}}"#,
        ],
        &[
            r#"{"kind":"outer","geometry":[{"type":"Polygon","coordinates":[[[0,0],[2,0],[2,2],[0,2],[0,0]]]},{"type":"Point","coordinates":[60,60]},{"type":"Polygon","coordinates":[]}]}"#,
        ],
        &[r#"{"kind":"inner","geometry":{"type":"LineString","coordinates":[[0.5,1],[1.5,1]]}}"#],
        &[r#"{"kind":"nogeo"}"#],
    ];
    for batch in batches {
        for json in *batch {
            writer.add_document(parse_document(&schema, json)?)?;
        }
        writer.commit()?;
    }
    writer.delete_term(Term::from_field_text(kind, "gone"));
    writer.commit()?;
    let reader = index.reader()?;
    assert!(reader
        .searcher()
        .segment_readers()
        .iter()
        .any(|segment| segment.num_deleted_docs() != 0));
    let parser = QueryParser::for_index(&index, vec![kind]);
    for merge in [false, true] {
        if merge {
            writer.merge(&index.searchable_segment_ids()?).wait()?;
            reader.reload()?;
        }
        for (text, count) in [
            ("kind:outer AND geometry:$contains($query(kind:inner))", 2),
            ("kind:gone AND geometry:$contains($query(kind:inner))", 0),
            ("kind:nogeo AND geometry:$contains($query(kind:inner))", 0),
            ("kind:inner AND geometry:$coveredby($query(kind:outer))", 1),
            ("kind:inner AND geometry:$coveredby($query(kind:gone))", 0),
            ("kind:inner AND geometry:$coveredby($query(kind:nogeo))", 0),
        ] {
            let query = parser.parse_query(text)?;
            assert_eq!(
                reader.searcher().search(&query, &Count)?,
                count,
                "{text}, merge {merge}"
            );
        }
    }
    Ok(())
}

#[test]
fn test_deleted_inner_geometry_does_not_match_join() -> tantivy::Result<()> {
    let mut builder = Schema::builder();
    let field = builder.add_custom_field("geometry", "spatial", json!({"index": "sphere"}));
    let kind = builder.add_text_field("kind", STRING);
    let schema = builder.build();
    let index = crate::create_index_in_ram(schema.clone())?;
    let mut writer: IndexWriter = index.writer_with_num_threads(1, 50_000_000)?;
    writer.set_merge_policy(Box::new(NoMergePolicy));
    for json in [
        r#"{"kind":"parcel","geometry":{"type":"Polygon","coordinates":[[[-0.01,-0.01],[0.01,-0.01],[0.01,0.01],[-0.01,0.01],[-0.01,-0.01]]]}}"#,
        r#"{"kind":"fiber","geometry":{"type":"LineString","coordinates":[[-0.005,0],[0.005,0]]}}"#,
    ] {
        writer.add_document(parse_document(&schema, json)?)?;
    }
    writer.commit()?;
    let reader = index.reader()?;
    let term = |text| TermQuery::new(Term::from_field_text(kind, text), IndexRecordOption::Basic);
    let join = SpatialExecutor::new(PlanNode::Join {
        field,
        outer: Box::new(PlanNode::Query(Box::new(term("parcel")))),
        inner: Box::new(PlanNode::Query(Box::new(term("fiber")))),
        relation: SpatialRelation::Near(61.0 / 6_371_000.0),
    });
    assert_eq!(reader.searcher().search(&join, &Count)?, 1);

    writer.delete_term(Term::from_field_text(kind, "fiber"));
    writer.commit()?;
    reader.reload()?;
    let searcher = reader.searcher();
    assert_eq!(searcher.segment_readers().len(), 1);
    let segment = &searcher.segment_readers()[0];
    // Keep the deleted fiber in the segment so the join must filter it out.
    assert_eq!((segment.max_doc(), segment.num_deleted_docs()), (2, 1));
    assert_eq!(searcher.search(&term("fiber"), &Count)?, 0);
    assert_eq!(searcher.search(&join, &Count)?, 0);
    Ok(())
}

#[test]
fn test_sphere_join_ignores_empty_query_members() -> tantivy::Result<()> {
    let mut builder = Schema::builder();
    let field = builder.add_custom_field("geometry", "spatial", json!({"index": "sphere"}));
    let kind = builder.add_text_field("kind", STRING);
    let schema = builder.build();
    let index = crate::create_index_in_ram(schema.clone())?;
    let mut writer: IndexWriter = index.writer_with_num_threads(1, 50_000_000)?;
    writer.set_merge_policy(Box::new(NoMergePolicy));
    for json in [
        r#"{"kind":"empty","geometry":{"type":"Polygon","coordinates":[]}}"#,
        r#"{"kind":"mixed","geometry":{"type":"GeometryCollection","geometries":[{"type":"Polygon","coordinates":[]},{"type":"Polygon","coordinates":[[[-1,-1],[1,-1],[1,1],[-1,1],[-1,-1]]]}]}}"#,
        r#"{"kind":"point","geometry":{"type":"Point","coordinates":[0,0]}}"#,
    ] {
        writer.add_document(parse_document(&schema, json)?)?;
    }
    writer.commit()?;
    let reader = index.reader()?;
    let term = |text| TermQuery::new(Term::from_field_text(kind, text), IndexRecordOption::Basic);
    let join = |outer| {
        SpatialExecutor::new(PlanNode::Join {
            field,
            outer: Box::new(PlanNode::Query(Box::new(term(outer)))),
            inner: Box::new(PlanNode::Query(Box::new(term("point")))),
            relation: SpatialRelation::Intersects,
        })
    };

    assert_eq!(reader.searcher().search(&join("empty"), &Count)?, 0);
    assert_eq!(reader.searcher().search(&join("mixed"), &Count)?, 1);
    Ok(())
}

#[test]
fn test_plane_predicates_and_joins_follow_field_surface() -> tantivy::Result<()> {
    use crate::query::{QueryParser, SpatialQuery};

    let mut builder = Schema::builder();
    let field = builder.add_custom_field("geometry", "spatial", json!({"index": "plane"}));
    let kind = builder.add_text_field("kind", STRING);
    let schema = builder.build();
    let index = crate::create_index_in_ram(schema.clone())?;
    let mut writer: IndexWriter = index.writer_with_num_threads(1, 50_000_000)?;
    writer.set_merge_policy(Box::new(NoMergePolicy));
    for json in [
        r#"{"kind":"area","geometry":{"type":"Polygon","coordinates":[[[0,0],[2,0],[2,2],[0,2],[0,0]]]}}"#,
        r#"{"kind":"interior","geometry":{"type":"LineString","coordinates":[[0.5,1],[1.5,1]]}}"#,
        r#"{"kind":"boundary","geometry":{"type":"LineString","coordinates":[[0.5,0],[1.5,0]]}}"#,
        r#"{"kind":"outside","geometry":{"type":"Point","coordinates":[5,5]}}"#,
    ] {
        writer.add_document(parse_document(&schema, json)?)?;
        writer.commit()?;
    }
    let reader = index.reader()?;
    let parser = QueryParser::for_index(&index, vec![kind]);
    let positive_distance = parser.parse_query("geometry:$within(1km, 1 1)")?;
    assert!(matches!(
        reader.searcher().search(&positive_distance, &Count),
        Err(tantivy::TantivyError::InvalidArgument(message))
            if message == "positive planar distance is not supported"
    ));
    let positive_distance = SpatialQuery::with_predicate(
        field,
        vec![[1.0, 1.0]],
        crate::query::SpatialPredicate::DistanceWithin(1.0),
    );
    assert!(matches!(
        reader.searcher().search(&positive_distance, &Count),
        Err(tantivy::TantivyError::InvalidArgument(message))
            if message == "positive planar distance is not supported"
    ));
    let positive_distance = SpatialExecutor::new(PlanNode::Within {
        field,
        geometry: json!({"type": "Point", "coordinates": [1.0, 1.0]}),
        radius: 1.0,
        filter: Box::new(PlanNode::Query(parser.parse_query("kind:area")?)),
    });
    assert!(matches!(
        reader.searcher().search(&positive_distance, &Count),
        Err(tantivy::TantivyError::InvalidArgument(message))
            if message == "positive planar distance is not supported"
    ));
    let checks = [
        ("kind:area AND geometry:$contains($query(kind:interior))", 1),
        ("kind:area AND geometry:$contains($query(kind:boundary))", 0),
        ("kind:area AND geometry:$covers($query(kind:boundary))", 1),
        (
            "kind:boundary AND geometry:$coveredby($query(kind:area))",
            1,
        ),
        (
            "kind:boundary AND geometry:$intersects($query(kind:area))",
            1,
        ),
        (
            "kind:boundary AND geometry:$within(0m, $query(kind:area))",
            1,
        ),
        (
            "kind:area AND NOT geometry:$contains($query(kind:boundary))",
            1,
        ),
        ("geometry:$contains(0.5 0.5, 1.5 0.5, 1 1.5)", 1),
        ("geometry:$covers(0.5 0.5, 1.5 0.5, 1 1.5)", 1),
        ("geometry:$coveredby(-1 -1, 3 -1, 3 3, -1 3)", 3),
        ("geometry:$intersects(-1 -1, 3 -1, 3 3, -1 3)", 3),
        ("geometry:$within(0m, 0.5 0)", 2),
    ];
    for merge in [false, true] {
        if merge {
            writer.merge(&index.searchable_segment_ids()?).wait()?;
            reader.reload()?;
        }
        for (text, count) in checks {
            let query = parser.parse_query(text)?;
            assert_eq!(
                reader.searcher().search(&query, &Count)?,
                count,
                "{text}, merge {merge}"
            );
        }
        let query = SpatialExecutor::new(PlanNode::Within {
            field,
            geometry: json!({"type": "Point", "coordinates": [0.5, 0.0]}),
            radius: 0.0,
            filter: Box::new(PlanNode::Query(parser.parse_query("kind:boundary")?)),
        });
        assert_eq!(reader.searcher().search(&query, &Count)?, 1);
    }
    writer.delete_term(Term::from_field_text(kind, "boundary"));
    writer.commit()?;
    reader.reload()?;
    let query = parser.parse_query("kind:area AND geometry:$covers($query(kind:boundary))")?;
    assert_eq!(reader.searcher().search(&query, &Count)?, 0);
    let query = parser.parse_query("kind:area AND geometry:$contains($query(kind:interior))")?;
    assert_eq!(reader.searcher().search(&query, &Count)?, 1);
    writer.delete_term(Term::from_field_text(kind, "area"));
    writer.commit()?;
    reader.reload()?;
    assert_eq!(reader.searcher().search(&query, &Count)?, 0);
    Ok(())
}

#[test]
fn test_sphere_covers_is_unsupported_before_search() -> tantivy::Result<()> {
    use crate::query::QueryParser;
    let mut builder = Schema::builder();
    builder.add_custom_field("geometry", "spatial", json!({"index": "sphere"}));
    let index = crate::create_index_in_ram(builder.build())?;
    let parser = QueryParser::for_index(&index, Vec::new());
    let query = parser.parse_query("geometry:$covers(0 0, 1 0, 1 1, 0 1)")?;
    assert!(matches!(
        index.reader()?.searcher().search(&query, &Count),
        Err(tantivy::TantivyError::InvalidArgument(message))
            if message == "Covers is not supported on this surface"
    ));
    Ok(())
}

#[test]
fn test_unsupported_join_validates_before_inputs() -> tantivy::Result<()> {
    #[derive(Clone, Debug)]
    struct UnreachableQuery;
    impl Query for UnreachableQuery {
        fn weight(&self, _: EnableScoring<'_>) -> tantivy::Result<Box<dyn Weight>> {
            panic!("unsupported join evaluated an input");
        }
    }
    for surface in ["plane", "sphere"] {
        let mut schema = Schema::builder();
        let field = schema.add_custom_field("geometry", "spatial", json!({"index": surface}));
        let index = crate::create_index_in_ram(schema.build())?;
        let relations = if surface == "plane" {
            vec![
                SpatialRelation::Near(1.0),
                SpatialRelation::Between(0.0, 1.0),
            ]
        } else {
            vec![
                SpatialRelation::Contains,
                SpatialRelation::Covers,
                SpatialRelation::CoveredBy,
                SpatialRelation::Between(0.0, 1.0),
            ]
        };
        for relation in relations {
            let query = SpatialExecutor::new(PlanNode::Join {
                field,
                outer: Box::new(PlanNode::Query(Box::new(UnreachableQuery))),
                inner: Box::new(PlanNode::Query(Box::new(UnreachableQuery))),
                relation,
            });
            assert!(matches!(index.reader()?.searcher().search(&query, &Count),
                Err(tantivy::TantivyError::InvalidArgument(message))
                    if message == "Spatial join relation is not supported on this surface"));
        }
    }
    Ok(())
}

#[test]
fn test_plan_literals_decode_source_geometry() -> tantivy::Result<()> {
    use tantivy::query::AllQuery;
    let mut schema = Schema::builder();
    let field = schema.add_custom_field("geometry", "spatial", json!({"index": "plane"}));
    let index = crate::create_index_in_ram(schema.build())?;
    let mut writer: IndexWriter = index.writer_with_num_threads(1, 15_000_000)?;
    writer.add_document(parse_document(
        &index.schema(),
        r#"{"geometry":{"type":"Polygon","coordinates":[[[0,0],[2,0],[2,2],[0,2],[0,0]]]}}"#,
    )?)?;
    writer.commit()?;
    let searcher = index.reader()?.searcher();
    for (geometry, expected) in [
        (
            json!({"type": "Polygon", "coordinates": [[[0.5,0.5],[1.5,0.5],[1,1.5],[0.5,0.5]]]}),
            1,
        ),
        (json!({"type": "LineString", "coordinates": [[1,1]]}), 1),
        (json!({"type": "Point", "coordinates": [0,95]}), 0),
    ] {
        let query = SpatialExecutor::new(PlanNode::Intersects {
            field,
            geometry,
            filter: Box::new(PlanNode::Query(Box::new(AllQuery))),
        });
        assert_eq!(searcher.search(&query, &Count)?, expected);
    }
    let invalid = SpatialExecutor::new(PlanNode::Intersects {
        field,
        geometry: json!({"type": "Point", "coordinates": "invalid"}),
        filter: Box::new(PlanNode::Query(Box::new(AllQuery))),
    });
    assert!(matches!(
        searcher.search(&invalid, &Count),
        Err(tantivy::TantivyError::InvalidArgument(_))
    ));
    Ok(())
}

#[test]
fn test_executor_rejects_old_cells() -> tantivy::Result<()> {
    use std::io::Write;
    use tantivy::directory::{CompositeFile, CompositeWrite, Directory};
    use tantivy::index::SegmentComponent;
    use tantivy::query::AllQuery;
    let mut schema = Schema::builder();
    let field = schema.add_custom_field("geometry", "spatial", json!({"index": "sphere"}));
    let index = crate::create_index_in_ram(schema.build())?;
    let mut writer: IndexWriter = index.writer_with_num_threads(1, 15_000_000)?;
    writer.add_document(tantivy::TantivyDocument::new())?;
    writer.commit()?;
    writer.wait_merging_threads()?;
    let segment = index.searchable_segments()?.pop().unwrap();
    for (extension, bytes) in [
        ("cells", &[0u8; 12][..]),
        ("edges", &[][..]),
        ("docids", &[][..]),
    ] {
        let component = SegmentComponent::Custom(extension.into());
        index
            .directory()
            .delete(&segment.relative_path(component.clone()))
            .unwrap();
        let mut output = CompositeWrite::wrap(segment.open_write(component)?);
        output.for_field(field).write_all(bytes)?;
        output.close()?;
    }
    let geometry = json!({"type": "Point", "coordinates": [0,0]});
    let all = || Box::new(PlanNode::Query(Box::new(AllQuery)));
    let searcher = index.reader()?.searcher();
    for node in [
        PlanNode::Intersects {
            field,
            geometry: geometry.clone(),
            filter: all(),
        },
        PlanNode::Within {
            field,
            geometry,
            radius: 0.01,
            filter: all(),
        },
        PlanNode::Join {
            field,
            outer: all(),
            inner: all(),
            relation: SpatialRelation::Intersects,
        },
        PlanNode::Join {
            field,
            outer: Box::new(PlanNode::Query(Box::new(tantivy::query::EmptyQuery))),
            inner: all(),
            relation: SpatialRelation::Intersects,
        },
    ] {
        let error = searcher
            .search(&SpatialExecutor::new(node), &Count)
            .unwrap_err();
        assert!(error.to_string().contains("rebuild"));
    }
    let file = CompositeFile::open(&segment.open_read(SegmentComponent::Custom("cells".into()))?)?;
    assert_eq!(
        file.open_read(field).unwrap().read_bytes()?.as_slice(),
        &[0; 12]
    );
    Ok(())
}

#[test]
#[should_panic(expected = "ring.is_empty()")]
fn test_plan_literal_preserves_ring_precondition() {
    use tantivy::query::AllQuery;
    let mut schema = Schema::builder();
    let field = schema.add_custom_field("geometry", "spatial", json!({"index": "plane"}));
    let index = crate::create_index_in_ram(schema.build()).unwrap();
    let query = SpatialExecutor::new(PlanNode::Intersects {
        field,
        geometry: json!({"type": "Polygon", "coordinates": [[[0,0],[1,0],[1,1]]]}),
        filter: Box::new(PlanNode::Query(Box::new(AllQuery))),
    });
    let _ = index.reader().unwrap().searcher().search(&query, &Count);
}

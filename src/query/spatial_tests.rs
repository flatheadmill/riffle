use serde_json::json;
use tantivy::collector::{Count, TopDocs};
use tantivy::merge_policy::NoMergePolicy;
use tantivy::schema::{STORED, STRING};
use tantivy::{IndexWriter, TantivyDocument, Term};

use super::*;
use crate::query::QueryParser;

fn compare_inputs<S: Surface>()
where S::Point: std::fmt::Debug {
    let ring = vec![[0.0, 0.0], [0.0, 1.0], [1.0, 1.0], [1.0, 0.0]];
    for mut ring in [ring.clone(), ring.into_iter().rev().collect()] {
        let coordinates = GeometryInput::Coordinates(&ring)
            .project::<S>(&SpatialPredicate::Intersects)
            .unwrap();
        ring.push(ring[0]);
        let value = json!({"type": "Polygon", "coordinates": [ring]});
        let geojson = GeometryInput::GeoJson(&value)
            .project::<S>(&SpatialPredicate::Intersects)
            .unwrap();
        assert_eq!(coordinates.members.len(), geojson.members.len());
        for (a, b) in coordinates.members.iter().zip(&geojson.members) {
            assert_eq!(a.kind, b.kind);
            assert_eq!(a.vertices, b.vertices);
            assert_eq!(a.ring_offsets, b.ring_offsets);
            assert_eq!(a.contains_hilbert_start, b.contains_hilbert_start);
        }
    }
    let coordinates = GeometryInput::Coordinates(&[[0.5, 0.5]])
        .project::<S>(&SpatialPredicate::DistanceWithin(0.0))
        .unwrap();
    assert_eq!(
        coordinates.members[0].kind,
        tantivy_spatial::geometry_set::GeometryKind::Point
    );
}

#[test]
fn test_query_projection_preserves_source_paths() {
    compare_inputs::<Sphere>();
    compare_inputs::<Plane>();
}

#[test]
fn test_literal_queries_after_delete_merge_and_reopen() -> tantivy::Result<()> {
    for surface in ["plane", "sphere"] {
        let directory = tempfile::tempdir()?;
        let mut schema = Schema::builder();
        let name = schema.add_text_field("name", STRING | STORED);
        let geometry = schema.add_custom_field("geometry", "spatial", json!({"index": surface}));
        let index = crate::create_index(directory.path(), schema.build())?;
        let mut writer: IndexWriter = index.writer_with_num_threads(1, 15_000_000)?;
        writer.set_merge_policy(Box::new(NoMergePolicy));
        let add = |writer: &mut IndexWriter, label: &str, value: Value| -> tantivy::Result<()> {
            let mut document = TantivyDocument::new();
            document.add_text(name, label);
            document.add_custom(geometry, &serde_json::to_vec(&value)?);
            writer.add_document(document)?;
            Ok(())
        };
        add(
            &mut writer,
            "oak",
            json!({"type": "Polygon", "coordinates": [[[0,0],[0,1],[1,1],[1,0],[0,0]]]}),
        )?;
        add(
            &mut writer,
            "pine",
            json!({"type": "Point", "coordinates": [3,3]}),
        )?;
        writer.commit()?;
        add(
            &mut writer,
            "elm",
            json!({"type": "Point", "coordinates": [0.5,0.5]}),
        )?;
        writer.commit()?;
        assert_eq!(index.searchable_segment_ids()?.len(), 2);
        let parser = QueryParser::for_index(&index, vec![name]);
        let query = parser.parse_query("geometry:$intersects(-1 -1, 2 -1, 2 2, -1 2)")?;
        let mixed =
            parser.parse_query("name:oak AND geometry:$intersects(-1 -1, 2 -1, 2 2, -1 2)^3")?;
        let text = parser.parse_query("name:oak")?;
        let searcher = index.reader()?.searcher();
        assert_eq!(searcher.search(query.as_ref(), &Count)?, 2);
        let point = parser
            .parse_query("name:elm AND geometry:$intersects(0.4 0.4, 0.6 0.4, 0.6 0.6, 0.4 0.6)")?;
        assert_eq!(searcher.search(point.as_ref(), &Count)?, 1);
        let collector = TopDocs::with_limit(10).order_by_score();
        let actual = searcher.search(mixed.as_ref(), &collector)?;
        let expected = searcher.search(text.as_ref(), &collector)?;
        assert_eq!(actual.len(), 1);
        assert_eq!(actual[0].1, expected[0].1);
        assert!((actual[0].0 - expected[0].0 - 3.0).abs() < 1e-5);
        for radius in [0.0, -1.0, f64::NAN] {
            let distance = SpatialQuery::with_predicate(
                geometry,
                vec![[0.5, 0.5]],
                SpatialPredicate::DistanceWithin(radius),
            );
            // Preserve the existing kernel behavior: Sphere excludes the coincident
            // point at nonpositive distance, while Plane includes it.
            let expected = if surface == "plane" { 2 } else { 1 };
            assert_eq!(
                searcher.search(&distance, &Count)?,
                expected,
                "{surface}: {radius}"
            );
        }
        let positive = parser.parse_query("geometry:$within(1km, 0.5 0.5)")?;
        if surface == "plane" {
            assert!(searcher.search(positive.as_ref(), &Count).is_err());
        } else {
            assert_eq!(searcher.search(positive.as_ref(), &Count)?, 2);
        }
        for text in ["geometry:$between(1m, 2m, 0 0)", "geometry:$knn(3, 0 0)"] {
            assert!(parser.parse_query(text).is_err());
        }
        drop(searcher);
        writer.delete_term(Term::from_field_text(name, "oak"));
        writer.commit()?;
        assert_eq!(
            index.reader()?.searcher().search(query.as_ref(), &Count)?,
            1
        );
        writer.merge(&index.searchable_segment_ids()?).wait()?;
        writer.wait_merging_threads()?;
        drop(index);
        let index = crate::open_index(directory.path())?;
        assert_eq!(index.searchable_segment_ids()?.len(), 1);
        assert_eq!(
            index.reader()?.searcher().search(query.as_ref(), &Count)?,
            1
        );
        let parser = QueryParser::for_index(&index, vec![name]);
        let parsed = parser.parse_query("geometry:$intersects(-1 -1, 2 -1, 2 2, -1 2)")?;
        assert_eq!(
            index.reader()?.searcher().search(parsed.as_ref(), &Count)?,
            1
        );
    }
    Ok(())
}

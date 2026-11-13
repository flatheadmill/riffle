use riffle::query::QueryParser;
use serde_json::{json, Value as JsonValue};
use tantivy::collector::DocSetCollector;
use tantivy::merge_policy::NoMergePolicy;
use tantivy::schema::{Field, Schema, Value, STORED, STRING};
use tantivy::{Index, IndexWriter, TantivyDocument, TantivyError, Term};

fn hit_ids(index: &Index, id: Field, query: &str) -> tantivy::Result<Vec<String>> {
    let parser = QueryParser::for_index(index, vec![id]);
    let query = parser.parse_query(query)?;
    let searcher = index.reader()?.searcher();
    let mut ids = Vec::new();
    for address in searcher.search(query.as_ref(), &DocSetCollector)? {
        let document: TantivyDocument = searcher.doc(address)?;
        ids.push(document.get_first(id).unwrap().as_str().unwrap().to_owned());
    }
    ids.sort();
    Ok(ids)
}

fn polygon(x: f64) -> JsonValue {
    json!({"type": "Polygon", "coordinates": [
        [[x, 0.0], [x + 2.0, 0.0], [x + 2.0, 2.0], [x, 2.0], [x, 0.0]]
    ]})
}

fn line(x: f64) -> JsonValue {
    json!({"type": "LineString", "coordinates": [[x + 0.5, 1.0], [x + 1.5, 1.0]]})
}

fn assert_survivors(index: &Index, id: Field) -> tantivy::Result<()> {
    assert_eq!(
        hit_ids(
            index,
            id,
            "kind:outer AND geometry:$intersects(-1 -1, 23 -1, 23 3, -1 3)"
        )?,
        ["outer-a", "outer-b", "outer-c"]
    );
    assert_eq!(
        hit_ids(
            index,
            id,
            "kind:outer AND geometry:$intersects($query(kind:inner))"
        )?,
        ["outer-a", "outer-c"]
    );
    assert!(hit_ids(index, id, "id:outer-deleted OR id:inner-deleted")?.is_empty());
    Ok(())
}

#[test]
fn test_spatial_queries_and_join_after_disk_reopen() -> tantivy::Result<()> {
    for surface in ["plane", "sphere"] {
        let directory = tempfile::tempdir()?;
        let mut schema = Schema::builder();
        let id = schema.add_text_field("id", STRING | STORED);
        let kind = schema.add_text_field("kind", STRING);
        let geometry = schema.add_custom_field("geometry", "spatial", json!({"index": surface}));
        {
            let index = riffle::create_opaque_index(directory.path(), schema.build())?;
            let mut writer: IndexWriter = index.writer_with_num_threads(1, 50_000_000)?;
            writer.set_merge_policy(Box::new(NoMergePolicy));
            // Put join partners and deleted documents in different source segments.
            for batch in [
                vec![
                    ("outer-deleted", "outer", polygon(0.0)),
                    ("outer-b", "outer", polygon(10.0)),
                    ("inner-c", "inner", line(20.0)),
                ],
                vec![
                    ("inner-deleted", "inner", line(10.0)),
                    ("outer-c", "outer", polygon(20.0)),
                ],
                vec![
                    ("outer-a", "outer", polygon(0.0)),
                    ("inner-a", "inner", line(0.0)),
                ],
            ] {
                for (label, category, shape) in batch {
                    let mut document = TantivyDocument::new();
                    document.add_text(id, label);
                    document.add_text(kind, category);
                    document.add_custom(geometry, &serde_json::to_vec(&shape)?);
                    writer.add_document(document)?;
                }
                writer.commit()?;
            }
            assert_eq!(index.searchable_segment_ids()?.len(), 3);
            assert_eq!(
                hit_ids(
                    &index,
                    id,
                    "kind:outer AND geometry:$intersects($query(kind:inner))"
                )?,
                ["outer-a", "outer-b", "outer-c", "outer-deleted"]
            );
            for label in ["outer-deleted", "inner-deleted"] {
                writer.delete_term(Term::from_field_text(id, label));
            }
            writer.commit()?;
            assert_survivors(&index, id)?;
            writer.merge(&index.searchable_segment_ids()?).wait()?;
            assert_eq!(index.searchable_segment_ids()?.len(), 1);
            writer.garbage_collect_files().wait()?;
            writer.wait_merging_threads()?;
            assert_survivors(&index, id)?;
        }

        {
            let unattached = Index::open_in_dir(directory.path())?;
            assert_eq!(
                hit_ids(&unattached, id, "kind:outer")?,
                ["outer-a", "outer-b", "outer-c"]
            );
            assert!(matches!(
                unattached.writer_with_num_threads::<TantivyDocument>(1, 50_000_000),
                Err(TantivyError::MissingPlugin(_))
            ));
        }
        let reopened = riffle::open_opaque_index(directory.path())?;
        let searcher = reopened.reader()?.searcher();
        assert_eq!(searcher.segment_readers().len(), 1);
        assert_eq!(searcher.segment_reader(0).max_doc(), 5);
        assert_eq!(searcher.segment_reader(0).num_deleted_docs(), 0);
        assert_survivors(&reopened, id)?;
    }
    Ok(())
}

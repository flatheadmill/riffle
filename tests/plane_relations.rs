use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use riffle::create_index_in_ram;
use tantivy::collector::DocSetCollector;
use tantivy::query::{SpatialPredicate, SpatialQuery};
use tantivy::schema::{Schema, Value, PLANE, STORED, STRING};
use tantivy::TantivyDocument;

fn retained_contacts() -> serde_json::Value {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("cases/plane_relations/michigan_contacts.json");
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

fn search(predicate: SpatialPredicate) -> BTreeSet<String> {
    let data = retained_contacts();
    let mut schema = Schema::builder();
    let id = schema.add_text_field("id", STRING | STORED);
    let geometry = schema.add_spatial_field("geometry", STORED | PLANE);
    let index = create_index_in_ram(schema.build()).unwrap();
    let mut writer = index.writer_with_num_threads(1, 50_000_000).unwrap();
    for (id, geometry) in data["cases"].as_object().unwrap() {
        let document = serde_json::json!({"id": id, "geometry": geometry});
        writer
            .add_document(
                TantivyDocument::parse_json(&index.schema(), &document.to_string()).unwrap(),
            )
            .unwrap();
    }
    writer.commit().unwrap();
    writer.wait_merging_threads().unwrap();

    let ring: Vec<[f64; 2]> =
        serde_json::from_value(data["query"]["coordinates"][0].clone()).unwrap();
    let query = SpatialQuery::with_predicate(geometry, ring, predicate);
    let reader = index.reader().unwrap();
    let searcher = reader.searcher();
    searcher
        .search(&query, &DocSetCollector)
        .unwrap()
        .into_iter()
        .map(|address| {
            let document: TantivyDocument = searcher.doc(address).unwrap();
            document.get_first(id).unwrap().as_str().unwrap().to_owned()
        })
        .collect()
}

#[test]
#[ignore = "GeoFabrik corpus characterization"]
fn test_michigan_plane_contacts() {
    assert_eq!(
        search(SpatialPredicate::Intersects),
        ["1469871", "2079965", "5775305"]
            .into_iter()
            .map(str::to_owned)
            .collect()
    );
    assert_eq!(
        search(SpatialPredicate::CoveredBy),
        ["1469871"].into_iter().map(str::to_owned).collect()
    );
}

use riffle::create_opaque_index;
use riffle::query::{SpatialPredicate, SpatialQuery};
use serde_json::{json, Value as JsonValue};
use tantivy::collector::TopDocs;
use tantivy::query::{BooleanQuery, Occur, QueryParser};
use tantivy::schema::{Field, Schema, Value, STORED, TEXT};
use tantivy::{DocAddress, IndexWriter, Score, TantivyDocument};

fn add_document(
    writer: &mut IndexWriter,
    name_field: Field,
    geometry_field: Field,
    name: &str,
    geometry: JsonValue,
) -> tantivy::Result<()> {
    let geometry = serde_json::to_vec(&geometry)?;
    let mut document = TantivyDocument::new();
    document.add_text(name_field, name);
    document.add_custom(geometry_field, &geometry);
    writer.add_document(document)?;
    Ok(())
}

fn main() -> tantivy::Result<()> {
    let directory = tempfile::tempdir()?;
    let mut schema = Schema::builder();
    let name = schema.add_text_field("name", TEXT | STORED);
    let geometry = schema.add_custom_field(
        "geometry",
        "spatial",
        json!({
            "index": "sphere",
        }),
    );
    let index = create_opaque_index(directory.path(), schema.build())?;

    let mut writer: IndexWriter = index.writer(50_000_000)?;
    add_document(
        &mut writer,
        name,
        geometry,
        "Kalkaska Muffler",
        json!({
            "type": "Point",
            "coordinates": [-85.1759, 44.7342],
        }),
    )?;
    add_document(
        &mut writer,
        name,
        geometry,
        "Traverse City Muffler",
        json!({
            "type": "Point",
            "coordinates": [-85.6206, 44.7631],
        }),
    )?;
    add_document(
        &mut writer,
        name,
        geometry,
        "Kalkaska Bakery",
        json!({
            "type": "Point",
            "coordinates": [-85.1765, 44.7338],
        }),
    )?;
    writer.commit()?;

    let bounds = json!({
        "type": "Polygon",
        "coordinates": [[
            [-85.20, 44.72],
            [-85.15, 44.72],
            [-85.15, 44.75],
            [-85.20, 44.75],
            [-85.20, 44.72],
        ]],
    });
    let spatial = SpatialQuery::from_geojson(geometry, bounds, SpatialPredicate::Intersects);

    let reader = index.reader()?;
    let searcher = reader.searcher();
    let text = QueryParser::for_index(&index, vec![name]).parse_query("muffler")?;
    let query = BooleanQuery::new(vec![(Occur::Must, text), (Occur::Must, Box::new(spatial))]);
    let top_docs: Vec<(Score, DocAddress)> =
        searcher.search(&query, &TopDocs::with_limit(10).order_by_score())?;

    for (_, address) in top_docs {
        let document: TantivyDocument = searcher.doc(address)?;
        let name = document
            .get_first(name)
            .and_then(|value| value.as_str())
            .unwrap();
        println!("{name}");
    }

    Ok(())
}

use std::fmt;

use common::OwnedBytes;
use riffle::{create_opaque_index, geometry_set_from_geojson};
use serde_json::{json, Value as JsonValue};
use tantivy::collector::TopDocs;
use tantivy::directory::CompositeFile;
use tantivy::index::SegmentComponent;
use tantivy::query::{
    BitSetDocSet, BooleanQuery, ConstScorer, EnableScoring, Explanation, Occur, Query, QueryParser,
    Scorer, Weight,
};
use tantivy::schema::{Field, Schema, Value, STORED, TEXT};
use tantivy::{
    DocAddress, DocId, DocSet, IndexWriter, Score, SegmentReader, TantivyDocument, TantivyError,
};
use tantivy_spatial::cell_index_reader::CellIndexReader;
use tantivy_spatial::edge_reader::EdgeReader;
use tantivy_spatial::edge_resolver::EdgeResolver;
use tantivy_spatial::geometry_set::GeometrySet;
use tantivy_spatial::intersects::Intersects;
use tantivy_spatial::region_coverer::CovererOptions;
use tantivy_spatial::sphere::Sphere;

#[derive(Clone)]
struct GeoIntersectsQuery {
    field: Field,
    geometry: GeometrySet<Sphere>,
}

impl fmt::Debug for GeoIntersectsQuery {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GeoIntersectsQuery")
            .field("field", &self.field)
            .finish_non_exhaustive()
    }
}

impl Query for GeoIntersectsQuery {
    fn weight(&self, _scoring: EnableScoring<'_>) -> tantivy::Result<Box<dyn Weight>> {
        Ok(Box::new(GeoIntersectsWeight {
            field: self.field,
            query: Intersects::new(self.geometry.clone(), CovererOptions::default()),
        }))
    }
}

struct GeoIntersectsWeight {
    field: Field,
    query: Intersects<Sphere>,
}

impl Weight for GeoIntersectsWeight {
    fn scorer(&self, segment: &SegmentReader, _boost: Score) -> tantivy::Result<Box<dyn Scorer>> {
        let cells = open_component(segment, "cells")?;
        let edges = open_component(segment, "edges")?;
        let docids = open_component(segment, "docids")?;

        let (Some(cells), Some(edges)) = (cells.open_read(self.field), edges.open_read(self.field))
        else {
            return Ok(Box::new(tantivy::query::EmptyScorer));
        };

        let cells = cells.read_bytes()?;
        let edges = edges.read_bytes()?;
        let docids = match docids.open_read(self.field) {
            Some(file) => file.read_bytes()?,
            None => OwnedBytes::empty(),
        };
        let cells = CellIndexReader::open_with_doc_ids(&cells, &docids)?;
        let edges = EdgeReader::<Sphere>::open(&edges)?;
        let mut edges = EdgeResolver::new(vec![edges]);
        let hits = self
            .query
            .search(&cells, None, &mut edges, segment.max_doc());

        Ok(Box::new(ConstScorer::new(BitSetDocSet::from(hits), 0.0)))
    }

    fn explain(&self, segment: &SegmentReader, doc: DocId) -> tantivy::Result<Explanation> {
        let mut scorer = self.scorer(segment, 1.0)?;
        if scorer.seek(doc) != doc {
            return Err(TantivyError::InvalidArgument(format!(
                "document {doc} does not match"
            )));
        }
        Ok(Explanation::new("GeoIntersectsQuery", 0.0))
    }
}

fn open_component(segment: &SegmentReader, extension: &str) -> tantivy::Result<CompositeFile> {
    let file = segment.open_read(SegmentComponent::Custom(extension.to_owned()))?;
    Ok(CompositeFile::open(&file)?)
}

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
    let spatial = GeoIntersectsQuery {
        field: geometry,
        geometry: geometry_set_from_geojson(0, &bounds).map_err(TantivyError::InvalidArgument)?,
    };

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

// Ingest newline-delimited GeoJSON into a tantivy index. Each line is a GeoJSON
// Feature with properties and geometry. No merging is performed; use the merge
// tool to consolidate segments afterward.
//
// Normalizes polygon winding order to RFC 7946: outer rings CCW, holes CW.

use std::fs::File;
use std::io::{BufRead, BufReader};

use clap::Parser;
use riffle::{create_opaque_index, open_opaque_index};
use tantivy::indexer::NoMergePolicy;
use tantivy::schema::{JsonObjectOptions, OwnedValue, Schema, STORED, STRING, TEXT};
use tantivy::{IndexWriter, TantivyDocument};

#[derive(Parser)]
#[command(about = "Ingest newline-delimited GeoJSON into a tantivy spatial index")]
struct Args {
    /// Path to the newline-delimited GeoJSON file.
    #[arg(long)]
    input: String,

    /// Directory to write the index. Created if it does not exist.
    #[arg(long)]
    dir: String,

    /// Documents per segment.
    #[arg(long, default_value_t = 100_000)]
    batch_size: usize,

    /// Maximum number of documents to ingest. Omit for no limit.
    #[arg(long)]
    limit: Option<usize>,

    /// Writer memory budget in gigabytes.
    #[arg(long, default_value_t = 1)]
    memory_gb: usize,

    /// Use planar (2D) indexing instead of unit sphere.
    #[arg(long)]
    plane: bool,
}

// Signed area of a ring using the shoelace formula. Positive means CCW.
fn signed_area(ring: &[serde_json::Value]) -> f64 {
    let n = ring.len();
    if n < 3 {
        return 0.0;
    }
    let mut sum = 0.0;
    for i in 0..n {
        let j = (i + 1) % n;
        let xi = ring[i][0].as_f64().unwrap_or(0.0);
        let yi = ring[i][1].as_f64().unwrap_or(0.0);
        let xj = ring[j][0].as_f64().unwrap_or(0.0);
        let yj = ring[j][1].as_f64().unwrap_or(0.0);
        sum += xi * yj - xj * yi;
    }
    sum / 2.0
}

// Normalize a polygon's rings: outer CCW, holes CW. Returns the number of rings reversed.
fn normalize_polygon(rings: &mut Vec<serde_json::Value>) -> usize {
    let mut reversed = 0;
    for (i, ring) in rings.iter_mut().enumerate() {
        let coords = match ring.as_array_mut() {
            Some(c) => c,
            None => continue,
        };
        let area = signed_area(coords);
        if i == 0 {
            // Outer ring should be CCW (positive area).
            if area < 0.0 {
                coords.reverse();
                reversed += 1;
            }
        } else {
            // Hole should be CW (negative area).
            if area > 0.0 {
                coords.reverse();
                reversed += 1;
            }
        }
    }
    reversed
}

// Normalize geometry in place. Returns the number of rings reversed.
fn normalize_geometry(geometry: &mut serde_json::Value) -> usize {
    let gtype = geometry
        .get("type")
        .and_then(|t| t.as_str())
        .unwrap_or("")
        .to_string();

    match gtype.as_str() {
        "Polygon" => {
            if let Some(rings) = geometry
                .get_mut("coordinates")
                .and_then(|c| c.as_array_mut())
            {
                return normalize_polygon(rings);
            }
            0
        }
        "MultiPolygon" => {
            let mut total = 0;
            if let Some(polygons) = geometry
                .get_mut("coordinates")
                .and_then(|c| c.as_array_mut())
            {
                for polygon in polygons.iter_mut() {
                    if let Some(rings) = polygon.as_array_mut() {
                        total += normalize_polygon(rings);
                    }
                }
            }
            total
        }
        _ => 0,
    }
}

fn main() -> tantivy::Result<()> {
    let args = Args::parse();

    let mut schema_builder = Schema::builder();
    let name_field = schema_builder.add_text_field("name", STRING | STORED);
    let properties_field =
        schema_builder.add_json_field("properties", JsonObjectOptions::from(TEXT | STORED));
    let geom_type_field = schema_builder.add_text_field("geom_type", STRING | STORED);
    let stored_geometry_field = schema_builder.add_bytes_field("_geometry", STORED);
    let index_name = if args.plane { "plane" } else { "sphere" };
    let geometry_field = schema_builder.add_custom_field(
        "geometry",
        "spatial",
        serde_json::json!({
            "index": index_name,
        }),
    );
    let schema = schema_builder.build();

    std::fs::create_dir_all(&args.dir).expect("could not create index directory");

    let index = if open_opaque_index(&args.dir).is_ok() {
        open_opaque_index(&args.dir)?
    } else {
        create_opaque_index(&args.dir, schema.clone())?
    };

    let memory_bytes = args.memory_gb * 1_073_741_824;
    let mut writer: IndexWriter = index.writer(memory_bytes)?;
    writer.set_merge_policy(Box::new(NoMergePolicy));

    let file = File::open(&args.input).expect("could not open input file");
    let reader = BufReader::new(file);

    let mut count = 0usize;
    let mut batch = 0usize;
    let mut skipped = 0usize;
    let mut rings_reversed = 0usize;
    let mut docs_with_reversed = 0usize;

    for line in reader.lines() {
        let line = line.expect("could not read line");
        if line.trim().is_empty() {
            continue;
        }
        let mut feature: serde_json::Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => {
                skipped += 1;
                continue;
            }
        };
        let obj = match feature.as_object_mut() {
            Some(o) => o,
            None => {
                skipped += 1;
                continue;
            }
        };
        let properties = obj
            .get("properties")
            .cloned()
            .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new()));
        let name = obj
            .get("properties")
            .and_then(|p| p.get("name"))
            .and_then(|n| n.as_str())
            .unwrap_or("")
            .to_string();
        let geometry = match obj.get_mut("geometry") {
            Some(g) => g,
            None => {
                skipped += 1;
                continue;
            }
        };
        let geom_type = geometry
            .get("type")
            .and_then(|t| t.as_str())
            .unwrap_or("")
            .to_string();

        let reversed = normalize_geometry(geometry);
        if reversed > 0 {
            rings_reversed += reversed;
            docs_with_reversed += 1;
        }

        let geometry_bytes = match serde_json::to_vec(geometry) {
            Ok(bytes) => bytes,
            Err(_) => {
                skipped += 1;
                continue;
            }
        };
        let properties = OwnedValue::from(properties);
        let mut doc = TantivyDocument::default();
        doc.add_text(name_field, name);
        doc.add_field_value(properties_field, &properties);
        doc.add_text(geom_type_field, geom_type);
        doc.add_bytes(stored_geometry_field, &geometry_bytes);
        doc.add_custom(geometry_field, &geometry_bytes);
        writer.add_document(doc)?;
        count += 1;

        if count % args.batch_size == 0 {
            writer.commit()?;
            batch += 1;
            eprintln!("committed batch {} ({} docs)", batch, count);
        }
        if let Some(limit) = args.limit {
            if count >= limit {
                break;
            }
        }
    }
    if count % args.batch_size != 0 {
        writer.commit()?;
        batch += 1;
        eprintln!("committed batch {} ({} docs)", batch, count);
    }

    writer.wait_merging_threads()?;

    let reader = index.reader()?;
    let searcher = reader.searcher();
    eprintln!(
        "{} segments, {} docs ingested, {} skipped",
        searcher.segment_readers().len(),
        count,
        skipped,
    );
    eprintln!(
        "{} rings reversed in {} documents",
        rings_reversed, docs_with_reversed,
    );

    Ok(())
}

// Query a point and print the names of geometries that contain it.

use clap::Parser;
use riffle::open_index;
use riffle::query::SpatialQuery;
use tantivy::collector::DocSetCollector;
use tantivy::schema::Value;
use tantivy::TantivyDocument;

#[derive(Parser)]
#[command(about = "Probe a point and list containing geometry names")]
#[command(allow_negative_numbers = true)]
struct Args {
    /// Path to an existing index directory.
    #[arg(long)]
    dir: String,

    /// Longitude.
    #[arg(long)]
    lon: f64,

    /// Latitude.
    #[arg(long)]
    lat: f64,

    /// Maximum results to print.
    #[arg(long, default_value_t = 20)]
    limit: usize,
}

fn main() -> tantivy::Result<()> {
    let args = Args::parse();

    let index = open_index(&args.dir)?;
    let schema = index.schema();
    let name_field = schema.get_field("name").unwrap();
    let geometry_field = schema.get_field("geometry").unwrap();

    let reader = index.reader()?;
    let searcher = reader.searcher();

    let epsilon = 0.001;
    let bounds = [
        [args.lon - epsilon, args.lat - epsilon],
        [args.lon + epsilon, args.lat + epsilon],
    ];
    let query = SpatialQuery::intersects_bounds(geometry_field, bounds);
    let doc_addresses = searcher.search(&query, &DocSetCollector)?;

    eprintln!("{} hits", doc_addresses.len());
    let mut printed = 0;
    for addr in &doc_addresses {
        let doc: TantivyDocument = searcher.doc(*addr)?;
        let name = doc
            .get_first(name_field)
            .and_then(|v| v.as_value().as_str().map(|s| s.to_string()))
            .unwrap_or_else(|| "(unnamed)".to_string());
        let name = if name.is_empty() {
            "(unnamed)".to_string()
        } else {
            name
        };
        println!("{}", name);
        printed += 1;
        if printed >= args.limit {
            if doc_addresses.len() > args.limit {
                eprintln!("... and {} more", doc_addresses.len() - args.limit);
            }
            break;
        }
    }

    Ok(())
}

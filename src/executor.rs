// Run a tantivy query against a spatial index. Supports text queries, spatial
// predicates, and spatial joins through the query parser.
//
// Examples:
//   --query 'properties.natural:water'
//   --query 'geometry:$intersects(-85.28 44.63, -85.08 44.63, -85.08 44.83, -85.28 44.83)'
//   --query 'geometry:$within(50mi, $query(properties.natural:water))'

use std::time::Instant;

use clap::Parser;
use riffle::open_index;
use riffle::query::QueryParser;
use tantivy::collector::{Count, DocSetCollector};
use tantivy::schema::Value;
use tantivy::TantivyDocument;

#[derive(clap::Parser)]
#[command(about = "Run a parsed query against a tantivy spatial index")]
struct Args {
    /// Path to an existing index directory.
    #[arg(long)]
    dir: String,

    /// Query string.
    #[arg(long)]
    query: String,

    /// Number of iterations.
    #[arg(long, default_value_t = 10)]
    iterations: usize,

    /// Print name and properties of each result.
    #[arg(long)]
    names: bool,

    /// Maximum results to print.
    #[arg(long, default_value_t = 20)]
    limit: usize,
}

fn main() -> tantivy::Result<()> {
    let args = Args::parse();

    let index = open_index(&args.dir)?;
    let schema = index.schema();

    let default_fields: Vec<_> = schema
        .fields()
        .filter_map(|(field, entry)| {
            if entry.is_indexed()
                && !matches!(entry.field_type(), tantivy::schema::FieldType::Custom(options)
                    if options.type_name() == "spatial")
            {
                Some(field)
            } else {
                None
            }
        })
        .collect();

    let query_parser = QueryParser::for_index(&index, default_fields);
    let query = query_parser
        .parse_query(&args.query)
        .expect("failed to parse query");

    let reader = index.reader()?;
    let searcher = reader.searcher();

    let hits = searcher.search(&query, &Count)?;
    eprintln!("{} hits", hits);

    let start = Instant::now();
    for _ in 0..args.iterations {
        searcher.search(&query, &Count)?;
    }
    let elapsed = start.elapsed();
    let per_query = elapsed / args.iterations as u32;
    eprintln!(
        "{} iterations in {:.3}s ({:.3}ms per query)",
        args.iterations,
        elapsed.as_secs_f64(),
        per_query.as_secs_f64() * 1000.0,
    );

    if args.names {
        let name_field = schema.get_field("name").ok();
        let props_field = schema.get_field("properties").ok();
        let type_field = schema.get_field("geom_type").ok();
        let doc_addrs = searcher.search(&query, &DocSetCollector)?;
        let mut printed = 0;
        for addr in doc_addrs {
            if printed >= args.limit {
                eprintln!("... ({} more)", hits - printed);
                break;
            }
            let doc: TantivyDocument = searcher.doc(addr)?;
            let name = name_field
                .and_then(|f| doc.get_first(f))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            let geom_type = type_field
                .and_then(|f| doc.get_first(f))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            eprintln!("{} [{}] doc_id={}", name, geom_type, addr.doc_id);
            printed += 1;
        }
    }

    Ok(())
}

use std::io::{BufWriter, Write};

use clap::Parser;
use riffle::open_index;
use riffle::spatial::stats::{sphere_stats_for_reader, SpatialStats};
use riffle::spatial::SpatialFieldReader;
use serde::Serialize;
use tantivy::ReloadPolicy;

#[derive(Parser)]
#[command(about = "Read spatial index shape statistics as JSON")]
struct Args {
    /// Path to an existing index directory.
    #[arg(long)]
    dir: String,

    /// Spatial field name.
    #[arg(long, default_value = "geometry")]
    field: String,
}

#[derive(Serialize)]
struct SegmentStats {
    index_path: String,
    field: String,
    segment_ord: usize,
    segment_id: String,
    max_doc: u32,
    num_deleted_docs: u32,
    stats: SpatialStats,
}

fn main() -> tantivy::Result<()> {
    let args = Args::parse();
    let index_path = std::fs::canonicalize(&args.dir)?;

    let index = open_index(&index_path)?;
    let schema = index.schema();
    let field = schema.get_field(&args.field)?;
    let reader = index
        .reader_builder()
        .reload_policy(ReloadPolicy::Manual)
        .try_into()?;
    let searcher = reader.searcher();

    let mut segments = Vec::new();
    let mut segments_visited = 0usize;
    let mut segments_skipped = 0usize;
    for (segment_ord, segment) in searcher.segment_readers().iter().enumerate() {
        segments_visited += 1;
        let Some(spatial_reader) = SpatialFieldReader::open(segment, field)? else {
            segments_skipped += 1;
            continue;
        };
        segments.push(SegmentStats {
            index_path: index_path.display().to_string(),
            field: args.field.clone(),
            segment_ord,
            segment_id: segment.segment_id().uuid_string(),
            max_doc: segment.max_doc(),
            num_deleted_docs: segment.num_deleted_docs(),
            stats: sphere_stats_for_reader(&spatial_reader)?,
        });
    }

    eprintln!(
        "stats: {} segments visited, {} emitted, {} skipped",
        segments_visited,
        segments.len(),
        segments_skipped
    );
    if segments.is_empty() {
        return Err(tantivy::TantivyError::InvalidArgument(format!(
            "no spatial segments matched field '{}'",
            args.field
        )));
    }

    let stdout = std::io::stdout();
    let mut stdout = BufWriter::new(stdout.lock());
    serde_json::to_writer_pretty(&mut stdout, &segments)?;
    writeln!(stdout)?;
    Ok(())
}

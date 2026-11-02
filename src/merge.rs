// Merge segments in an index directory. Segments are merged in groups, repeating
// until the target segment count is reached.

use clap::Parser;
use fabricator::open_index;
use tantivy::index::SegmentId;
use tantivy::indexer::NoMergePolicy;
use tantivy::IndexWriter;

#[derive(Parser)]
#[command(about = "Merge segments in a tantivy index directory")]
struct Args {
    /// Path to an existing index directory.
    #[arg(long)]
    dir: String,

    /// Stop merging when this many segments remain.
    #[arg(long, default_value_t = 1)]
    target: usize,

    /// Number of segments to merge at once.
    #[arg(long, default_value_t = 2)]
    segments: usize,

    /// Stop after this many rounds of merging.
    #[arg(long)]
    rounds: Option<usize>,

    /// Number of indexing threads.
    #[arg(long, default_value_t = 1)]
    threads: usize,
}

fn main() -> tantivy::Result<()> {
    let args = Args::parse();

    let index = open_index(&args.dir)?;
    let mut rounds_remaining = args.rounds;

    loop {
        let reader = index.reader()?;
        let searcher = reader.searcher();
        let segment_ids: Vec<SegmentId> = searcher
            .segment_readers()
            .iter()
            .map(|s| s.segment_id())
            .collect();
        let n = segment_ids.len();
        eprintln!("{} segments", n);

        if n <= args.target {
            break;
        }
        drop(searcher);
        drop(reader);

        let mut writer: IndexWriter =
            index.writer_with_num_threads(args.threads, args.threads * 15_000_000)?;
        writer.set_merge_policy(Box::new(NoMergePolicy));

        let mut i = 0;
        let mut merges = Vec::new();
        while i < n {
            let remaining = n - i;
            let group_size = if remaining <= args.segments + 1 && remaining > args.segments {
                remaining
            } else {
                std::cmp::min(args.segments, remaining)
            };
            if group_size < 2 {
                break;
            }
            let group: Vec<SegmentId> = segment_ids[i..i + group_size].to_vec();
            eprintln!("merging {} segments: {:?}", group.len(), group);
            merges.push(writer.merge(&group));
            i += group_size;
        }

        for merge in merges {
            match merge.wait()? {
                Some(meta) => eprintln!("  -> {}", meta.id().uuid_string()),
                None => eprintln!("  -> no output"),
            }
        }

        writer.wait_merging_threads()?;

        if let Some(ref mut remaining) = rounds_remaining {
            *remaining -= 1;
            if *remaining == 0 {
                let reader = index.reader()?;
                let searcher = reader.searcher();
                eprintln!(
                    "{} segments after {} rounds",
                    searcher.segment_readers().len(),
                    args.rounds.unwrap()
                );
                break;
            }
        }
    }

    Ok(())
}

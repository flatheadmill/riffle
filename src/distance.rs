// Query a spatial index for geometries within a distance of a bounding box.
// The distance is edge-to-edge: from the nearest edge of the bounding box to
// the nearest edge of each candidate geometry.

use std::time::Instant;

use clap::Parser;
use riffle::open_index;
use riffle::spatial::SpatialFieldReader;
use tantivy_spatial::cell_index_reader::CellIndexReader;
use tantivy_spatial::distance::Distance;
use tantivy_spatial::edge_reader::EdgeReader;
use tantivy_spatial::edge_resolver::EdgeResolver;
use tantivy_spatial::geometry_set::GeometrySet;
use tantivy_spatial::region_coverer::CovererOptions;
use tantivy_spatial::sphere::Sphere;
use tantivy_spatial::surface::Surface;

const EARTH_RADIUS_METERS: f64 = 6_371_000.0;

#[derive(Parser)]
#[command(about = "Find geometries within a distance of a bounding box")]
#[command(allow_negative_numbers = true)]
struct Args {
    /// Path to an existing index directory.
    #[arg(long)]
    dir: String,

    /// Longitude low.
    #[arg(long)]
    lon_lo: f64,

    /// Latitude low.
    #[arg(long)]
    lat_lo: f64,

    /// Longitude high.
    #[arg(long)]
    lon_hi: f64,

    /// Latitude high.
    #[arg(long)]
    lat_hi: f64,

    /// Maximum distance in meters (edge-to-edge).
    #[arg(long)]
    radius: f64,
}

fn main() -> tantivy::Result<()> {
    let args = Args::parse();

    let ring = vec![
        [args.lon_lo, args.lat_lo],
        [args.lon_hi, args.lat_lo],
        [args.lon_hi, args.lat_hi],
        [args.lon_lo, args.lat_hi],
        [args.lon_lo, args.lat_lo],
    ];
    let mut set = GeometrySet::<Sphere>::new(0);
    set.push_polygon(&[ring
        .iter()
        .map(|point| Sphere::project(point[0], point[1]))
        .collect()]);

    let radius_radians = args.radius / EARTH_RADIUS_METERS;
    let query = Distance::<Sphere>::new(set, radius_radians, CovererOptions::default());

    let index = open_index(&args.dir)?;
    let schema = index.schema();
    let field = schema.get_field("geometry").unwrap();

    let reader = index.reader()?;
    let searcher = reader.searcher();

    let start = Instant::now();
    let mut total_hits = 0usize;

    for seg in searcher.segment_readers() {
        let spatial = SpatialFieldReader::open(seg, field)?;
        if let Some(spatial_reader) = spatial {
            let cell_reader = CellIndexReader::open(spatial_reader.cells_bytes())?;
            let edge_reader = EdgeReader::<Sphere>::open(spatial_reader.edges_bytes())?;
            let mut edge_resolver = EdgeResolver::new(vec![edge_reader]);

            let bitset = query.search(&cell_reader, None, &mut edge_resolver, seg.max_doc());
            total_hits += bitset.len();
        }
    }

    let elapsed = start.elapsed();
    eprintln!(
        "{} hits in {:.3}ms",
        total_hits,
        elapsed.as_secs_f64() * 1000.0
    );

    Ok(())
}

use std::path::Path;
use std::sync::Arc;

use tantivy::schema::Schema;
use tantivy::spatial::plane::Plane;
use tantivy::spatial::sphere::Sphere;
use tantivy::spatial::{Cons, NativeSpatial, Nil, S2Plugin, SurfaceIndex};
use tantivy::{Index, SegmentPlugin};

mod spatial_input;

pub use spatial_input::{geometry_set_from_geojson, OpaqueGeoJson};

type SpatialIndexes = Cons<SurfaceIndex<Sphere>, Cons<SurfaceIndex<Plane>, Nil>>;

/// Create the spatial segment plugin used by Riffle indexes.
pub fn spatial_plugin() -> Arc<dyn SegmentPlugin> {
    Arc::new(S2Plugin::<SpatialIndexes, NativeSpatial>::new())
}

/// Create the spatial segment plugin for opaque GeoJSON fields.
pub fn opaque_spatial_plugin() -> Arc<dyn SegmentPlugin> {
    Arc::new(S2Plugin::<SpatialIndexes, OpaqueGeoJson>::new())
}

/// Create an index with Riffle's spatial component attached.
pub fn create_index<P: AsRef<Path>>(path: P, schema: Schema) -> tantivy::Result<Index> {
    Index::builder()
        .schema(schema)
        .register_plugin(spatial_plugin())
        .create_in_dir(path)
}

/// Create an in-memory index with Riffle's spatial component attached.
pub fn create_index_in_ram(schema: Schema) -> tantivy::Result<Index> {
    Index::builder()
        .schema(schema)
        .register_plugin(spatial_plugin())
        .create_in_ram()
}

/// Create an index whose spatial fields contain opaque GeoJSON.
pub fn create_opaque_index<P: AsRef<Path>>(path: P, schema: Schema) -> tantivy::Result<Index> {
    Index::builder()
        .schema(schema)
        .register_plugin(opaque_spatial_plugin())
        .create_in_dir(path)
}

/// Open an index and attach Riffle's spatial component.
pub fn open_index<P: AsRef<Path>>(path: P) -> tantivy::Result<Index> {
    let mut index = Index::open_in_dir(path)?;
    index.register_plugin(spatial_plugin());
    Ok(index)
}

/// Open an index whose spatial fields contain opaque GeoJSON.
pub fn open_opaque_index<P: AsRef<Path>>(path: P) -> tantivy::Result<Index> {
    let mut index = Index::open_in_dir(path)?;
    index.register_plugin(opaque_spatial_plugin());
    Ok(index)
}

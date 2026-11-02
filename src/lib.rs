use std::path::Path;
use std::sync::Arc;

use tantivy::schema::Schema;
use tantivy::spatial::plane::Plane;
use tantivy::spatial::sphere::Sphere;
use tantivy::spatial::{Cons, Nil, S2Plugin, SurfaceIndex};
use tantivy::{Index, SegmentPlugin};

type SpatialIndexes = Cons<SurfaceIndex<Sphere>, Cons<SurfaceIndex<Plane>, Nil>>;

/// Create the spatial segment plugin used by Fabricator indexes.
pub fn spatial_plugin() -> Arc<dyn SegmentPlugin> {
    Arc::new(S2Plugin::<SpatialIndexes>::new())
}

/// Create an index with Fabricator's spatial component attached.
pub fn create_index<P: AsRef<Path>>(path: P, schema: Schema) -> tantivy::Result<Index> {
    Index::builder()
        .schema(schema)
        .register_plugin(spatial_plugin())
        .create_in_dir(path)
}

/// Create an in-memory index with Fabricator's spatial component attached.
pub fn create_index_in_ram(schema: Schema) -> tantivy::Result<Index> {
    Index::builder()
        .schema(schema)
        .register_plugin(spatial_plugin())
        .create_in_ram()
}

/// Open an index and attach Fabricator's spatial component.
pub fn open_index<P: AsRef<Path>>(path: P) -> tantivy::Result<Index> {
    let mut index = Index::open_in_dir(path)?;
    index.register_plugin(spatial_plugin());
    Ok(index)
}

use std::path::Path;
use std::sync::Arc;

use tantivy::schema::Schema;
use tantivy::{Index, SegmentPlugin};

mod spatial_input;
mod spatial_plugin;

pub use spatial_input::{geometry_set_from_geojson, OpaqueGeoJson};
use spatial_plugin::RiffleSpatialPlugin;

/// Create the spatial segment plugin used by Riffle indexes.
pub fn spatial_plugin() -> Arc<dyn SegmentPlugin> {
    Arc::new(RiffleSpatialPlugin)
}

/// Create the spatial segment plugin for opaque GeoJSON fields.
pub fn opaque_spatial_plugin() -> Arc<dyn SegmentPlugin> {
    Arc::new(RiffleSpatialPlugin)
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

pub use query_grammar;

#[cfg(test)]
mod query_grammar_tests;

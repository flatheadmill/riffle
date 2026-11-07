use common::OwnedBytes;
use tantivy::directory::CompositeFile;
use tantivy::index::SegmentComponent;
use tantivy::schema::Field;
use tantivy::SegmentReader;

/// Mapped component slices for one spatial field in a segment.
pub struct SpatialFieldReader {
    cells: OwnedBytes,
    edges: OwnedBytes,
    doc_ids: OwnedBytes,
}

impl SpatialFieldReader {
    pub fn open(reader: &SegmentReader, field: Field) -> tantivy::Result<Option<Self>> {
        let cells = open_component(reader, "cells")?;
        let edges = open_component(reader, "edges")?;
        let doc_ids = open_component(reader, "docids")?;
        let (Some(cells), Some(edges)) = (cells.open_read(field), edges.open_read(field)) else {
            return Ok(None);
        };
        Ok(Some(Self {
            cells: cells.read_bytes()?,
            edges: edges.read_bytes()?,
            doc_ids: match doc_ids.open_read(field) {
                Some(file) => file.read_bytes()?,
                None => OwnedBytes::empty(),
            },
        }))
    }

    pub fn cells_bytes(&self) -> &[u8] {
        &self.cells
    }
    pub fn edges_bytes(&self) -> &[u8] {
        &self.edges
    }
    pub fn doc_ids_bytes(&self) -> &[u8] {
        &self.doc_ids
    }
}

fn open_component(reader: &SegmentReader, extension: &str) -> tantivy::Result<CompositeFile> {
    let file = reader.open_read(SegmentComponent::Custom(extension.to_owned()))?;
    Ok(CompositeFile::open(&file)?)
}

pub mod stats;

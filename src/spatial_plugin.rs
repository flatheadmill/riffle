use std::any::Any;
use std::collections::HashMap;

use tantivy::directory::{CompositeFile, CompositeWrite, WritePtr};
use tantivy::index::SegmentComponent;
use tantivy::plugin::{PluginMergeContext, PluginWriter, PluginWriterContext, SegmentPlugin};
use tantivy::schema::document::{ErasedDocument, ErasedValue};
use tantivy::schema::{Field, Schema};
use tantivy::{DocId, Segment, TantivyError};
use tantivy_spatial::field_writer::SurfaceFieldWriter;
use tantivy_spatial::merge_field::{merge_field, MergeSource};
use tantivy_spatial::plane::Plane;
use tantivy_spatial::sphere::Sphere;

use crate::spatial_input::OpaqueGeoJson;

pub(crate) struct RiffleSpatialPlugin;

enum FieldWriter {
    Sphere(SurfaceFieldWriter<Sphere>),
    Plane(SurfaceFieldWriter<Plane>),
}

impl FieldWriter {
    fn from_name(name: &str) -> tantivy::Result<Self> {
        match name {
            "sphere" => Ok(Self::Sphere(SurfaceFieldWriter::new())),
            "plane" => Ok(Self::Plane(SurfaceFieldWriter::new())),
            _ => Err(TantivyError::InvalidArgument(format!(
                "spatial index '{name}' is not registered"
            ))),
        }
    }

    fn add_value(&mut self, doc_id: DocId, value: ErasedValue<'_>) -> tantivy::Result<()> {
        match self {
            Self::Sphere(writer) => {
                if let Some(set) = OpaqueGeoJson::convert::<Sphere>(doc_id, value)? {
                    writer.add_set(set);
                }
            }
            Self::Plane(writer) => {
                if let Some(set) = OpaqueGeoJson::convert::<Plane>(doc_id, value)? {
                    writer.add_set(set);
                }
            }
        }
        Ok(())
    }

    fn mem_usage(&self) -> usize {
        match self {
            Self::Sphere(writer) => writer.mem_usage(),
            Self::Plane(writer) => writer.mem_usage(),
        }
    }

    fn serialize(
        self,
        cells: &mut common::CountingWriter<WritePtr>,
        edges: &mut common::CountingWriter<WritePtr>,
        doc_ids: &mut common::CountingWriter<WritePtr>,
        old_to_new: Option<&[DocId]>,
    ) -> tantivy::Result<()> {
        match self {
            Self::Sphere(writer) => writer.serialize(cells, edges, doc_ids, old_to_new)?,
            Self::Plane(writer) => writer.serialize(cells, edges, doc_ids, old_to_new)?,
        }
        Ok(())
    }
}

struct RiffleSpatialWriter {
    fields: HashMap<Field, FieldWriter>,
}

impl RiffleSpatialWriter {
    fn new(schema: &Schema) -> tantivy::Result<Self> {
        let mut fields = HashMap::new();
        for (field, entry) in schema.fields() {
            if let Some(name) = OpaqueGeoJson::index_name(entry)? {
                fields.insert(field, FieldWriter::from_name(name)?);
            }
        }
        Ok(Self { fields })
    }
}

impl PluginWriter for RiffleSpatialWriter {
    fn add_document(
        &mut self,
        doc_id: DocId,
        document: &dyn ErasedDocument,
        _schema: &Schema,
    ) -> tantivy::Result<()> {
        for (field, value) in document.erased_fields() {
            if let Some(writer) = self.fields.get_mut(&field) {
                writer.add_value(doc_id, value)?;
            }
        }
        Ok(())
    }

    fn serialize(
        self: Box<Self>,
        segment: &Segment,
        doc_id_map: Option<&tantivy::indexer::doc_id_mapping::DocIdMapping>,
    ) -> tantivy::Result<()> {
        let mut cells = CompositeWrite::wrap(open_write(segment, "cells")?);
        let mut edges = CompositeWrite::wrap(open_write(segment, "edges")?);
        let mut doc_ids = CompositeWrite::wrap(open_write(segment, "docids")?);
        for (field, writer) in self.fields {
            writer.serialize(
                cells.for_field(field),
                edges.for_field(field),
                doc_ids.for_field(field),
                doc_id_map.map(|mapping| mapping.old_to_new_ids()),
            )?;
        }
        cells.close()?;
        edges.close()?;
        doc_ids.close()?;
        Ok(())
    }

    fn mem_usage(&self) -> usize {
        self.fields.values().map(FieldWriter::mem_usage).sum()
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

impl SegmentPlugin for RiffleSpatialPlugin {
    fn extensions(&self) -> &[&str] {
        &["cells", "edges", "docids"]
    }

    fn create_writer(&self, ctx: &PluginWriterContext) -> tantivy::Result<Box<dyn PluginWriter>> {
        Ok(Box::new(RiffleSpatialWriter::new(&ctx.segment.schema())?))
    }

    fn merge(&self, ctx: PluginMergeContext) -> tantivy::Result<()> {
        let mut cells = CompositeWrite::wrap(open_write(ctx.target_segment, "cells")?);
        let mut edges = CompositeWrite::wrap(open_write(ctx.target_segment, "edges")?);
        let mut doc_ids = CompositeWrite::wrap(open_write(ctx.target_segment, "docids")?);
        let mut doc_id_inverse = ctx
            .readers
            .iter()
            .map(|reader| vec![None; reader.max_doc() as usize])
            .collect::<Vec<_>>();
        for (new_doc_id, old_addr) in ctx.doc_id_mapping.iter_old_doc_addrs().enumerate() {
            doc_id_inverse[old_addr.segment_ord as usize][old_addr.doc_id as usize] =
                Some(new_doc_id as DocId);
        }

        let source_files = ctx
            .readers
            .iter()
            .map(|reader| {
                Ok((
                    CompositeFile::open(&reader.open_read(custom("cells"))?)?,
                    CompositeFile::open(&reader.open_read(custom("edges"))?)?,
                ))
            })
            .collect::<tantivy::Result<Vec<_>>>()?;

        for (field, entry) in ctx.schema.fields() {
            let Some(name) = OpaqueGeoJson::index_name(entry)? else {
                continue;
            };
            let owned = source_files
                .iter()
                .map(|(cells, edges)| {
                    let cells = cells
                        .open_read(field)
                        .map(|file| file.read_bytes())
                        .transpose()?;
                    let edges = edges
                        .open_read(field)
                        .map(|file| file.read_bytes())
                        .transpose()?;
                    Ok((cells, edges))
                })
                .collect::<tantivy::Result<Vec<_>>>()?;
            let sources = owned
                .iter()
                .enumerate()
                .map(|(ordinal, (cells, edges))| MergeSource {
                    cells_bytes: cells.as_deref().unwrap_or(&[]),
                    edges_bytes: edges.as_deref().unwrap_or(&[]),
                    segment_name: ctx.readers[ordinal].segment_id().uuid_string(),
                })
                .collect::<Vec<_>>();
            match name {
                "sphere" => merge_field::<Sphere, _, _, _>(
                    &sources,
                    ctx.doc_id_mapping.alive_bitsets(),
                    cells.for_field(field),
                    edges.for_field(field),
                    doc_ids.for_field(field),
                    &doc_id_inverse,
                )?,
                "plane" => merge_field::<Plane, _, _, _>(
                    &sources,
                    ctx.doc_id_mapping.alive_bitsets(),
                    cells.for_field(field),
                    edges.for_field(field),
                    doc_ids.for_field(field),
                    &doc_id_inverse,
                )?,
                _ => {
                    return Err(TantivyError::InvalidArgument(format!(
                        "spatial index '{name}' is not registered"
                    )))
                }
            }
        }
        cells.close()?;
        edges.close()?;
        doc_ids.close()?;
        Ok(())
    }
}

fn custom(extension: &str) -> SegmentComponent {
    SegmentComponent::Custom(extension.to_owned())
}

fn open_write(segment: &Segment, extension: &str) -> tantivy::Result<WritePtr> {
    Ok(segment.open_write(custom(extension))?)
}

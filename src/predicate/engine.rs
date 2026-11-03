use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use common::BitSet;
use riffle::{create_index, open_index};
use serde_json::json;
use tantivy::indexer::NoMergePolicy;
use tantivy::schema::{Field, Schema, Value, PLANE, SPHERE, STORED, STRING};
use tantivy::spatial::cell_index_reader::CellIndexReader;
use tantivy::spatial::clip_options::ClipOptions;
use tantivy::spatial::clipper::Clipper;
use tantivy::spatial::edge_reader::EdgeReader;
use tantivy::spatial::edge_resolver::EdgeResolver;
use tantivy::spatial::geometry::Geometry;
use tantivy::spatial::geometry_set::GeometrySet;
use tantivy::spatial::intersects::Intersects;
use tantivy::spatial::plane::Plane;
use tantivy::spatial::region_coverer::CovererOptions;
use tantivy::spatial::sphere::Sphere;
use tantivy::spatial::surface::Surface;
use tantivy::spatial::to_geometry_set;
use tantivy::spatial::within::Within;
use tantivy::{DocAddress, Index, IndexWriter, ReloadPolicy, Searcher, TantivyDocument};

use super::case::{Case, Filter, Predicate, SurfaceKind};
use super::protocol::{
    self, ClipperSettings, Completed, Context, Diagnostics, Header, Identity, Observed, Result,
    Seed, Stage, StoredId, WorkerError, WorkerRequest,
};

pub fn build(case: &Case, path: &Path) -> Result<Index> {
    let mut schema = Schema::builder();
    schema.add_text_field("id", STRING | STORED);
    match case.surface {
        SurfaceKind::Plane => schema.add_spatial_field("geometry", STORED | PLANE),
        SurfaceKind::Sphere => schema.add_spatial_field("geometry", STORED | SPHERE),
    };
    fs::create_dir(path)?;
    let index = create_index(path, schema.build())?;
    let mut writer: IndexWriter = index.writer_with_num_threads(1, 50_000_000)?;
    writer.set_merge_policy(Box::new(NoMergePolicy));
    for doc in &case.documents {
        let raw = json!({"id": doc.id, "geometry": doc.geometry});
        let document = TantivyDocument::parse_json(&index.schema(), &raw.to_string())?;
        writer.add_document(document)?;
    }
    writer.commit()?;
    writer.wait_merging_threads()?;
    drop(index);
    Ok(open_index(path)?)
}

pub fn recover_ids(searcher: &Searcher, field: Field, expected: &[&str]) -> Result<Vec<StoredId>> {
    if searcher.segment_readers().len() != 1 {
        return Err("worker requires one physical segment".into());
    }
    let segment = &searcher.segment_readers()[0];
    if segment.num_deleted_docs() != 0 || segment.max_doc() as usize != expected.len() {
        return Err("ingested document count or deletion state differs from the case".into());
    }
    let expected_set: BTreeSet<_> = expected.iter().copied().collect();
    if expected_set.len() != expected.len() {
        return Err("expected document IDs are not unique".into());
    }
    let mut stored_ids = Vec::with_capacity(expected.len());
    let mut actual_set = BTreeSet::new();
    for doc_id in 0..segment.max_doc() {
        let stored: TantivyDocument = searcher.doc(DocAddress::new(0, doc_id))?;
        let values: Vec<_> = stored.get_all(field).collect();
        if values.len() != 1 {
            return Err("document must have exactly one stored ID".into());
        }
        let id = values[0]
            .as_str()
            .ok_or("stored ID is not a string")?
            .to_owned();
        if !actual_set.insert(id.clone()) {
            return Err("duplicate stored document ID".into());
        }
        stored_ids.push(StoredId { doc_id, id });
    }
    if actual_set
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>()
        != expected_set
    {
        return Err("stored document IDs differ from the case".into());
    }
    Ok(stored_ids)
}

fn query_set<S: Surface>(case: &Case) -> Result<GeometrySet<S>> {
    let raw = serde_json::to_value(&case.query)?;
    let geometry =
        Geometry::<Plane>::from_geojson(raw.as_object().ok_or("query is not an object")?)
            .map_err(|e| format!("invalid query geometry: {e:?}"))?;
    Ok(to_geometry_set(&geometry.project::<S>(), 0))
}

fn query_seeds<S: Surface>(
    set: &GeometrySet<S>,
    members: &[usize],
    cells: &CellIndexReader<'_>,
    options: &ClipOptions,
) -> Result<Vec<Seed>> {
    // These cells supply diagnostics only. Each query constructs its own index.
    let clipped = Clipper::new(options.clone()).build(std::slice::from_ref(set));
    let mut seeds = Vec::new();
    for cell in &clipped.cells {
        let (start, end) = cells.range_for_cell(cell.cell_id, 0, cells.cell_count());
        let first_index_level = if start < end {
            Some(cells.cell_id_at(start).level())
        } else {
            None
        };
        for shape in &cell.shapes {
            let member = set
                .members
                .get(shape.geometry_id.1 as usize)
                .ok_or("clipped query references an absent member")?;
            seeds.push(Seed {
                cell_id: cell.cell_id.0.to_string(),
                query_level: cell.cell_id.level(),
                member: *members
                    .get(shape.geometry_id.1 as usize)
                    .ok_or("query member mapping is absent")? as u32,
                contains_center: shape.contains_center,
                edge_indices: shape.edge_indices.clone(),
                member_vertices: member.vertices.len(),
                member_zero_vertices: set
                    .members
                    .first()
                    .ok_or("query has no members")?
                    .vertices
                    .len(),
                index_start: start,
                index_end: end,
                first_index_level,
            });
        }
    }
    Ok(seeds)
}

fn direct<S: Surface + Clone>(
    case: &Case,
    searcher: &Searcher,
    root: &Path,
    context: &Context,
    header: Header,
    stage: &mut Stage,
) -> Result<Vec<String>> {
    let schema = searcher.schema();
    let ids = recover_ids(
        searcher,
        schema.get_field("id")?,
        &case
            .documents
            .iter()
            .map(|d| d.id.as_str())
            .collect::<Vec<_>>(),
    )?;
    let segment = &searcher.segment_readers()[0];
    let spatial = segment
        .spatial_fields()
        .get_field(schema.get_field("geometry")?)?
        .ok_or("spatial field is absent after ingestion")?;
    let available = !spatial.doc_ids_bytes().is_empty();
    if case.sidecar && !available {
        return Err("requested cell doc-id sidecar is absent".into());
    }
    let cells = if case.sidecar {
        CellIndexReader::open_with_doc_ids(spatial.cells_bytes(), spatial.doc_ids_bytes())
    } else {
        CellIndexReader::open(spatial.cells_bytes())
    }?;
    let mut mask = BitSet::with_max_value(segment.max_doc());
    let mut selected_ids = Vec::new();
    for entry in &ids {
        if case.filter.selected(&entry.id) {
            mask.insert(entry.doc_id);
            selected_ids.push(entry.id.clone());
        }
    }
    selected_ids.sort();
    let set = query_set::<S>(case)?;
    let options = ClipOptions::default();
    let seeds = match case.predicate {
        Predicate::Intersects => query_seeds(
            &set,
            &(0..set.members.len()).collect::<Vec<_>>(),
            &cells,
            &options,
        )?,
        Predicate::CoveredBy => {
            let mut order: Vec<_> = (0..set.members.len()).collect();
            order.sort_by(|&a, &b| {
                set.members[b]
                    .vertices
                    .len()
                    .cmp(&set.members[a].vertices.len())
            });
            let mut seeds = Vec::new();
            for member in order {
                let polygon = GeometrySet {
                    members: vec![set.members[member].clone()],
                    doc_id: set.doc_id,
                };
                seeds.extend(query_seeds(&polygon, &[member], &cells, &options)?);
            }
            seeds
        }
    };
    let diagnostics = Diagnostics {
        header,
        segment_count: searcher.segment_readers().len(),
        segment_id: segment.segment_id().uuid_string(),
        max_doc: segment.max_doc(),
        deleted_docs: segment.num_deleted_docs(),
        stored_ids: ids.clone(),
        filter: case.filter.clone(),
        selected_ids,
        sidecar_requested: case.sidecar,
        sidecar_available: available,
        sidecar_used: !cells.doc_ids_data.is_empty(),
        cell_count: cells.cell_count(),
        clipper: ClipperSettings {
            max_edges_per_cell: options.max_edges_per_cell,
            min_short_edge_fraction: options.min_short_edge_fraction,
        },
        seeds,
    };
    protocol::write_json(&root.join("diagnostics.json"), &diagnostics)?;
    *stage = Stage::Query;
    protocol::progress(root, context, *stage)?;
    let mut edge_resolver = EdgeResolver::new(vec![EdgeReader::<S>::open(spatial.edges_bytes())?]);
    let filter = if matches!(case.filter, Filter::None {}) {
        None
    } else {
        Some(&mask)
    };
    let hits = match case.predicate {
        Predicate::Intersects => Intersects::new(set, CovererOptions::default()).search(
            &cells,
            filter,
            &mut edge_resolver,
            segment.max_doc(),
        ),
        Predicate::CoveredBy => Within::new(set, CovererOptions::default()).search(
            &cells,
            filter,
            &mut edge_resolver,
            segment.max_doc(),
        ),
    };
    let mut actual: Vec<_> = ids
        .into_iter()
        .filter(|entry| hits.contains(entry.doc_id))
        .map(|entry| entry.id)
        .collect();
    actual.sort();
    Ok(actual)
}

pub fn run(
    request: WorkerRequest,
    argv: Vec<String>,
    compiled: Identity,
) -> std::result::Result<(), String> {
    let root = Path::new(&request.output).to_owned();
    // The parent owns the containing directory and captures stdout and stderr.
    fs::create_dir(&root).map_err(|e| format!("cannot create worker output: {e}"))?;
    let mut context = Context {
        requested: request,
        compiled,
        observed: Observed { case_sha256: None },
        identity_verified: None,
        argv,
    };
    let mut stage = Stage::Validate;
    let result = (|| -> Result<()> {
        protocol::progress(&root, &context, stage)?;
        let bytes = fs::read(&context.requested.case)?;
        context.observed.case_sha256 = Some(protocol::sha256(&bytes));
        let verified = context.verify();
        context.identity_verified = Some(verified.is_ok());
        protocol::progress(&root, &context, stage)?;
        let header = verified?;
        let case: Case = serde_json::from_slice(&bytes)?;
        case.validate_structure()
            .map_err(|e| format!("invalid case: {e:?}"))?;
        case.validate_supported()
            .map_err(|e| format!("unsupported case: {e:?}"))?;
        stage = Stage::Index;
        protocol::progress(&root, &context, stage)?;
        let index = build(&case, &root.join("index"))?;
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::Manual)
            .try_into()?;
        let searcher = reader.searcher();
        stage = Stage::Diagnostics;
        protocol::progress(&root, &context, stage)?;
        let actual = match case.surface {
            SurfaceKind::Plane => direct::<Plane>(
                &case,
                &searcher,
                &root,
                &context,
                header.clone(),
                &mut stage,
            )?,
            SurfaceKind::Sphere => direct::<Sphere>(
                &case,
                &searcher,
                &root,
                &context,
                header.clone(),
                &mut stage,
            )?,
        };
        protocol::write_json(&root.join("result.json"), &Completed { header, actual })?;
        stage = Stage::Result;
        protocol::progress(&root, &context, stage)?;
        Ok(())
    })();
    result.map_err(|e| {
        let message = e.to_string();
        let error = WorkerError {
            context,
            stage,
            message: message.clone(),
        };
        match protocol::write_json(&root.join("error.json"), &error) {
            Ok(()) => message,
            Err(write_error) => format!("{message}; cannot write error record: {write_error}"),
        }
    })
}

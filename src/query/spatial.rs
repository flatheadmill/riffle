use std::io;
use std::marker::PhantomData;

use common::BitSet;
use serde_json::Value;
use tantivy::query::{
    BitSetDocSet, ConstScorer, EnableScoring, Explanation, Query, Scorer, Weight,
};
use tantivy::schema::{Field, Schema};
use tantivy::{DocId, DocSet, Score, SegmentReader, TantivyError};
use tantivy_spatial::cell_index_reader::CellIndexReader;
use tantivy_spatial::contains::Contains;
use tantivy_spatial::distance::Distance;
use tantivy_spatial::edge_reader::EdgeReader;
use tantivy_spatial::edge_resolver::EdgeResolver;
use tantivy_spatial::geometry_set::GeometrySet;
use tantivy_spatial::intersects::Intersects;
use tantivy_spatial::plane::query::PlaneQuery;
use tantivy_spatial::plane::{Plane, Predicate};
use tantivy_spatial::region_coverer::CovererOptions;
use tantivy_spatial::sphere::Sphere;
use tantivy_spatial::surface::Surface;
use tantivy_spatial::within::Within;

use super::SpatialRelation;
use crate::spatial::SpatialFieldReader;
use crate::spatial_input::OpaqueGeoJson;

#[derive(Clone, Debug)]
pub enum SpatialPredicate {
    Contains,
    Covers,
    Intersects,
    CoveredBy,
    DistanceWithin(f64),
}

#[derive(Clone, Debug)]
enum SourceGeometry {
    Coordinates(Vec<[f64; 2]>),
    GeoJson(Value),
}

#[derive(Clone, Copy)]
pub(crate) enum GeometryInput<'a> {
    Coordinates(&'a [[f64; 2]]),
    GeoJson(&'a Value),
}

impl GeometryInput<'_> {
    fn project<S: Surface>(self, predicate: &SpatialPredicate) -> tantivy::Result<GeometrySet<S>> {
        match self {
            Self::GeoJson(value) => {
                crate::geometry_set_from_geojson(0, value).map_err(TantivyError::InvalidArgument)
            }
            Self::Coordinates(coordinates) => {
                let mut ring = coordinates.to_vec();
                if ring.first() != ring.last() {
                    ring.push(ring[0]);
                }
                let mut set = GeometrySet::new(0);
                if matches!(predicate, SpatialPredicate::DistanceWithin(_)) && ring.len() == 1 {
                    set.push_point(S::project(ring[0][0], ring[0][1]));
                } else {
                    let ring = ring
                        .iter()
                        .map(|point| S::project(point[0], point[1]))
                        .collect();
                    set.push_polygon(&[ring]);
                }
                Ok(set)
            }
        }
    }
}

/// A literal spatial predicate over a custom spatial field.
#[derive(Clone, Debug)]
pub struct SpatialQuery {
    field: Field,
    predicate: SpatialPredicate,
    geometry: SourceGeometry,
}

impl SpatialQuery {
    pub fn new(field: Field, polygon: Vec<[f64; 2]>) -> Self {
        Self::with_predicate(field, polygon, SpatialPredicate::Contains)
    }

    pub fn with_predicate(
        field: Field,
        coordinates: Vec<[f64; 2]>,
        predicate: SpatialPredicate,
    ) -> Self {
        Self {
            field,
            predicate,
            geometry: SourceGeometry::Coordinates(coordinates),
        }
    }

    pub fn from_geojson(field: Field, geometry: Value, predicate: SpatialPredicate) -> Self {
        Self {
            field,
            predicate,
            geometry: SourceGeometry::GeoJson(geometry),
        }
    }

    pub fn from_bounds(field: Field, bounds: [[f64; 2]; 2]) -> Self {
        Self::with_predicate(field, bounds_ring(bounds), SpatialPredicate::Contains)
    }

    pub fn intersects_bounds(field: Field, bounds: [[f64; 2]; 2]) -> Self {
        Self::with_predicate(field, bounds_ring(bounds), SpatialPredicate::Intersects)
    }
}

fn bounds_ring([lo, hi]: [[f64; 2]; 2]) -> Vec<[f64; 2]> {
    vec![
        [lo[0], lo[1]],
        [hi[0], lo[1]],
        [hi[0], hi[1]],
        [lo[0], hi[1]],
    ]
}

impl Query for SpatialQuery {
    fn weight(&self, scoring: EnableScoring<'_>) -> tantivy::Result<Box<dyn Weight>> {
        let searcher = scoring
            .searcher()
            .expect("searcher required for spatial query");
        let index = field_index(&searcher.index().schema(), self.field)?;
        let geometry = match &self.geometry {
            SourceGeometry::Coordinates(coordinates) => GeometryInput::Coordinates(coordinates),
            SourceGeometry::GeoJson(value) => GeometryInput::GeoJson(value),
        };
        Ok(Box::new(SpatialWeight {
            field: self.field,
            query: index.prepare(geometry, &self.predicate)?,
        }))
    }
}

struct SpatialWeight {
    field: Field,
    query: Box<dyn PreparedSpatialQuery>,
}

impl Weight for SpatialWeight {
    fn scorer(&self, reader: &SegmentReader, boost: Score) -> tantivy::Result<Box<dyn Scorer>> {
        let hits = match SpatialFieldReader::open(reader, self.field)? {
            Some(field) => self.query.search(&field, reader.max_doc(), None)?,
            None => BitSet::with_max_value(reader.max_doc()),
        };
        Ok(Box::new(ConstScorer::new(BitSetDocSet::from(hits), boost)))
    }

    fn explain(&self, reader: &SegmentReader, doc: DocId) -> tantivy::Result<Explanation> {
        let mut scorer = self.scorer(reader, 1.0)?;
        if scorer.seek(doc) != doc {
            return Err(TantivyError::InvalidArgument(format!(
                "document {doc} does not match"
            )));
        }
        Ok(Explanation::new("SpatialQuery", scorer.score()))
    }
}

pub(crate) trait PreparedSpatialQuery: Send + Sync {
    fn search(
        &self,
        field: &SpatialFieldReader,
        max_doc: u32,
        filter: Option<&BitSet>,
    ) -> io::Result<BitSet>;
}

pub(crate) trait SpatialIndex: Send + Sync {
    fn prepare(
        &self,
        geometry: GeometryInput<'_>,
        predicate: &SpatialPredicate,
    ) -> tantivy::Result<Box<dyn PreparedSpatialQuery>>;

    fn value_heads(&self, edges: &[u8], doc_id: u32, heads: &mut Vec<u32>) -> tantivy::Result<()>;

    fn prepare_join_query(
        &self,
        edges: &[u8],
        head: u32,
        relation: &SpatialRelation,
    ) -> tantivy::Result<Box<dyn PreparedSpatialQuery>>;

    fn validate_join(&self, relation: &SpatialRelation) -> tantivy::Result<()>;
}

pub(crate) struct SurfaceIndex<S>(PhantomData<S>);

pub(crate) fn field_index(schema: &Schema, field: Field) -> tantivy::Result<Box<dyn SpatialIndex>> {
    let name = OpaqueGeoJson::index_name(schema.get_field_entry(field))?
        .ok_or_else(|| TantivyError::InvalidArgument("field is not spatial".into()))?;
    match name {
        "sphere" => Ok(Box::new(SurfaceIndex::<Sphere>(PhantomData))),
        "plane" => Ok(Box::new(SurfaceIndex::<Plane>(PhantomData))),
        _ => Err(TantivyError::InvalidArgument(format!(
            "spatial index '{name}' not registered"
        ))),
    }
}

impl<S: Surface + Send + Sync + 'static> SpatialIndex for SurfaceIndex<S> {
    fn prepare(
        &self,
        geometry: GeometryInput<'_>,
        predicate: &SpatialPredicate,
    ) -> tantivy::Result<Box<dyn PreparedSpatialQuery>> {
        match predicate {
            SpatialPredicate::Covers if S::DIMENSIONS != 2 => {
                return Err(TantivyError::InvalidArgument(
                    "Covers is not supported on this surface".into(),
                ))
            }
            SpatialPredicate::DistanceWithin(radius) if S::DIMENSIONS == 2 && *radius > 0.0 => {
                return Err(TantivyError::InvalidArgument(
                    "positive planar distance is not supported".into(),
                ))
            }
            _ => {}
        }
        let set = geometry.project::<S>(predicate)?;
        let options = CovererOptions::default();
        let query = match predicate {
            SpatialPredicate::Intersects => {
                PreparedQuery::Intersects(Intersects::new(set, options))
            }
            SpatialPredicate::Contains => PreparedQuery::Contains(Contains::new(set, options)),
            SpatialPredicate::CoveredBy => PreparedQuery::Within(Within::new(set, options)),
            SpatialPredicate::Covers => {
                PreparedQuery::Plane(PlaneQuery::prepare(&set).unwrap(), Predicate::Covers, false)
            }
            SpatialPredicate::DistanceWithin(radius) => {
                PreparedQuery::Distance(Distance::new(set, *radius, options))
            }
        };
        Ok(Box::new(query))
    }

    fn value_heads(&self, edges: &[u8], doc_id: u32, heads: &mut Vec<u32>) -> tantivy::Result<()> {
        heads.extend(EdgeReader::<S>::open(edges)?.value_heads(doc_id)?);
        Ok(())
    }

    fn prepare_join_query(
        &self,
        edges: &[u8],
        head: u32,
        relation: &SpatialRelation,
    ) -> tantivy::Result<Box<dyn PreparedSpatialQuery>> {
        self.validate_join(relation)?;
        let (_, set) = EdgeReader::<S>::open(edges)?.read_geometry_set(head);
        let options = CovererOptions::default();
        let query = match relation {
            SpatialRelation::Intersects if S::DIMENSIONS == 2 => PreparedQuery::Plane(
                PlaneQuery::prepare(&set).unwrap(),
                Predicate::Intersects,
                true,
            ),
            SpatialRelation::Intersects => PreparedQuery::Intersects(Intersects::new(set, options)),
            SpatialRelation::Near(radius) => {
                if S::DIMENSIONS == 2 && *radius > 0.0 {
                    return Err(TantivyError::InvalidArgument(
                        "positive planar distance is not implemented".into(),
                    ));
                }
                PreparedQuery::Distance(Distance::any_within(set, *radius, options))
            }
            SpatialRelation::Contains | SpatialRelation::Covers | SpatialRelation::CoveredBy => {
                let query = PlaneQuery::prepare(&set).ok_or_else(|| {
                    TantivyError::InvalidArgument(
                        "Coverage joins are not supported on this surface".into(),
                    )
                })?;
                // The probe evaluates (inner, outer), so reverse the outer-first relation.
                let predicate = match relation {
                    SpatialRelation::Contains => Predicate::Within,
                    SpatialRelation::Covers => Predicate::CoveredBy,
                    SpatialRelation::CoveredBy => Predicate::Covers,
                    _ => unreachable!(),
                };
                PreparedQuery::Plane(query, predicate, true)
            }
            SpatialRelation::Between(_, _) => {
                return Err(TantivyError::InvalidArgument(
                    "Between joins are not supported".into(),
                ))
            }
        };
        Ok(Box::new(query))
    }

    fn validate_join(&self, relation: &SpatialRelation) -> tantivy::Result<()> {
        let supported = match relation {
            SpatialRelation::Intersects => true,
            SpatialRelation::Near(radius) => S::DIMENSIONS == 3 || !(*radius > 0.0),
            SpatialRelation::Contains | SpatialRelation::Covers | SpatialRelation::CoveredBy => {
                S::DIMENSIONS == 2
            }
            SpatialRelation::Between(_, _) => false,
        };
        if supported {
            Ok(())
        } else {
            Err(TantivyError::InvalidArgument(
                "Spatial join relation is not supported on this surface".into(),
            ))
        }
    }
}

pub(crate) enum PreparedQuery<S: Surface> {
    Intersects(Intersects<S>),
    Contains(Contains<S>),
    Within(Within<S>),
    Distance(Distance<S>),
    Plane(PlaneQuery, Predicate, bool),
}

impl<S: Surface + 'static> PreparedSpatialQuery for PreparedQuery<S> {
    fn search(
        &self,
        field: &SpatialFieldReader,
        max_doc: u32,
        filter: Option<&BitSet>,
    ) -> io::Result<BitSet> {
        let cells = CellIndexReader::open_with_doc_ids(field.cells_bytes(), field.doc_ids_bytes())?;
        let mut edges = EdgeResolver::new(vec![EdgeReader::<S>::open(field.edges_bytes())?]);
        Ok(match self {
            Self::Intersects(query) => query.search(&cells, filter, &mut edges, max_doc),
            Self::Contains(query) => query.search(&cells, filter, &mut edges, max_doc),
            Self::Within(query) => query.search(&cells, filter, &mut edges, max_doc),
            Self::Distance(query) => query.search(&cells, filter, &mut edges, max_doc),
            Self::Plane(query, predicate, first_only) => {
                query.search(&cells, filter, &edges, max_doc, *predicate, *first_only)
            }
        })
    }
}

#[cfg(test)]
#[path = "spatial_tests.rs"]
mod tests;

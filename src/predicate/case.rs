use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

pub const LONGHAND_VERSION: &str = "intersects-uv-v1";
pub const COVERED_BY_VERSION: &str = "covered-by-disjoint-uv-v1";
// UV coordinates stay inside 0.95. This floor keeps ordinary clearance checks
// above projection roundoff; it is not an exact-arithmetic error bound.
pub const MIN_MARGIN: f64 = 1e-12;
pub type Point = [f64; 2];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Predicate {
    #[default]
    Intersects,
    CoveredBy,
}

impl Predicate {
    pub fn longhand_version(self) -> &'static str {
        match self {
            Self::Intersects => LONGHAND_VERSION,
            Self::CoveredBy => COVERED_BY_VERSION,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SurfaceKind {
    Plane,
    Sphere,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Family {
    Point,
    MultiPoint,
    LineString,
    MultiLineString,
    Polygon,
    MultiPolygon,
    GeometryCollection,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", deny_unknown_fields)]
pub enum Geometry {
    Point { coordinates: Point },
    MultiPoint { coordinates: Vec<Point> },
    LineString { coordinates: Vec<Point> },
    MultiLineString { coordinates: Vec<Vec<Point>> },
    Polygon { coordinates: Vec<Vec<Point>> },
    MultiPolygon { coordinates: Vec<Vec<Vec<Point>>> },
    GeometryCollection { geometries: Vec<Geometry> },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "scope", rename_all = "snake_case")]
pub enum Location {
    Case { path: String },
    Query { path: String },
    Document { id: String, path: String },
}

impl Location {
    pub fn case(path: &str) -> Self {
        Self::Case { path: path.into() }
    }

    pub fn at(&self, suffix: &str) -> Self {
        match self {
            Self::Case { path } => Self::Case {
                path: format!("{path}{suffix}"),
            },
            Self::Query { path } => Self::Query {
                path: format!("{path}{suffix}"),
            },
            Self::Document { id, path } => Self::Document {
                id: id.clone(),
                path: format!("{path}{suffix}"),
            },
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InvalidReason {
    EmptyName,
    Margin,
    EmptyDocuments,
    DocumentId,
    FilterIds,
    ExpectedIds,
    Cardinality,
    RingClosure,
    Coordinate,
    ZeroEdge,
    SelfIntersection,
    CollapsedRing,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnsupportedReason {
    CaseVersion,
    LonghandVersion,
    QueryMembersOverlap,
    Family,
    Holes,
    Face,
    MarginFloor,
    Size,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AmbiguousReason {
    ShortEdge,
    CollinearVertex,
    RingClearance,
    BoundaryIncidence,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Reason {
    Invalid { reason: InvalidReason },
    Unsupported { reason: UnsupportedReason },
    Ambiguous { reason: AmbiguousReason },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rejection {
    pub location: Location,
    pub family: Option<Family>,
    #[serde(flatten)]
    pub reason: Reason,
}

impl Rejection {
    pub fn invalid(location: Location, family: Option<Family>, reason: InvalidReason) -> Self {
        Self {
            location,
            family,
            reason: Reason::Invalid { reason },
        }
    }

    pub fn unsupported(
        location: Location,
        family: Option<Family>,
        reason: UnsupportedReason,
    ) -> Self {
        Self {
            location,
            family,
            reason: Reason::Unsupported { reason },
        }
    }
}

impl Geometry {
    pub fn family(&self) -> Family {
        match self {
            Self::Point { .. } => Family::Point,
            Self::MultiPoint { .. } => Family::MultiPoint,
            Self::LineString { .. } => Family::LineString,
            Self::MultiLineString { .. } => Family::MultiLineString,
            Self::Polygon { .. } => Family::Polygon,
            Self::MultiPolygon { .. } => Family::MultiPolygon,
            Self::GeometryCollection { .. } => Family::GeometryCollection,
        }
    }

    fn validate(&self, location: &Location) -> Result<(), Rejection> {
        let family = self.family();
        let invalid = |location, reason| Rejection::invalid(location, Some(family), reason);
        let position = |&[lon, lat]: &Point, location: &Location| {
            if !lon.is_finite() || !lat.is_finite() || lon.abs() > 180.0 || lat.abs() > 90.0 {
                Err(invalid(location.clone(), InvalidReason::Coordinate))
            } else {
                Ok(())
            }
        };
        let positions = |points: &[Point], min: usize, location: &Location| {
            if points.len() < min {
                return Err(invalid(location.clone(), InvalidReason::Cardinality));
            }
            for (i, point) in points.iter().enumerate() {
                position(point, &location.at(&format!("/{i}")))?;
            }
            Ok(())
        };
        let polygon = |rings: &[Vec<Point>], location: &Location| {
            if rings.is_empty() {
                return Err(invalid(location.clone(), InvalidReason::Cardinality));
            }
            for (i, ring) in rings.iter().enumerate() {
                let at = location.at(&format!("/{i}"));
                positions(ring, 4, &at)?;
                if ring.first() != ring.last() {
                    return Err(invalid(at, InvalidReason::RingClosure));
                }
            }
            Ok(())
        };
        let coordinates = location.at("/coordinates");
        match self {
            Self::Point { coordinates: point } => position(point, &coordinates),
            Self::MultiPoint {
                coordinates: points,
            } => positions(points, 1, &coordinates),
            Self::LineString {
                coordinates: points,
            } => positions(points, 2, &coordinates),
            Self::MultiLineString { coordinates: lines } => {
                if lines.is_empty() {
                    return Err(invalid(coordinates, InvalidReason::Cardinality));
                }
                for (i, line) in lines.iter().enumerate() {
                    positions(line, 2, &coordinates.at(&format!("/{i}")))?;
                }
                Ok(())
            }
            Self::Polygon { coordinates: rings } => polygon(rings, &coordinates),
            Self::MultiPolygon {
                coordinates: polygons,
            } => {
                if polygons.is_empty() {
                    return Err(invalid(coordinates, InvalidReason::Cardinality));
                }
                for (i, rings) in polygons.iter().enumerate() {
                    polygon(rings, &coordinates.at(&format!("/{i}")))?;
                }
                Ok(())
            }
            Self::GeometryCollection { geometries } => {
                if geometries.is_empty() {
                    return Err(invalid(
                        location.at("/geometries"),
                        InvalidReason::Cardinality,
                    ));
                }
                for (i, geometry) in geometries.iter().enumerate() {
                    geometry.validate(&location.at(&format!("/geometries/{i}")))?;
                }
                Ok(())
            }
        }
    }

    pub fn supported(&self, location: &Location) -> Result<(), Rejection> {
        let unsupported =
            |location, reason| Rejection::unsupported(location, Some(self.family()), reason);
        let ring = |rings: &[Vec<Point>], location: Location| {
            if rings.len() != 1 {
                Err(unsupported(location, UnsupportedReason::Holes))
            } else if rings[0].len() > 4096 {
                Err(unsupported(location, UnsupportedReason::Size))
            } else {
                Ok(())
            }
        };
        match self {
            Self::Point { .. } => Ok(()),
            Self::Polygon { coordinates } => ring(coordinates, location.at("/coordinates")),
            Self::MultiPolygon { coordinates } => {
                if coordinates.len() > 64
                    || coordinates.iter().flatten().map(Vec::len).sum::<usize>() > 4096
                {
                    return Err(unsupported(location.clone(), UnsupportedReason::Size));
                }
                for (i, rings) in coordinates.iter().enumerate() {
                    ring(rings, location.at(&format!("/coordinates/{i}")))?;
                }
                Ok(())
            }
            _ => Err(unsupported(location.clone(), UnsupportedReason::Family)),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Document {
    pub id: String,
    pub geometry: Geometry,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum Filter {
    None {},
    All {},
    Include { ids: Vec<String> },
    Exclude { ids: Vec<String> },
}

impl Filter {
    pub fn selected(&self, id: &str) -> bool {
        match self {
            Self::None {} | Self::All {} => true,
            Self::Include { ids } => ids.iter().any(|entry| entry == id),
            Self::Exclude { ids } => !ids.iter().any(|entry| entry == id),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Expected {
    pub longhand_version: String,
    pub ids: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Case {
    pub version: u32,
    pub name: String,
    #[serde(default)]
    pub predicate: Predicate,
    pub surface: SurfaceKind,
    pub documents: Vec<Document>,
    pub query: Geometry,
    pub filter: Filter,
    pub sidecar: bool,
    pub margin: f64,
    pub expected: Expected,
}

impl Case {
    pub fn validate_structure(&self) -> Result<(), Rejection> {
        let invalid = |path, reason| Rejection::invalid(Location::case(path), None, reason);
        let unsupported = |path, reason| Rejection::unsupported(Location::case(path), None, reason);
        if self.version != 1 {
            return Err(unsupported("/version", UnsupportedReason::CaseVersion));
        }
        if self.expected.longhand_version != self.predicate.longhand_version() {
            return Err(unsupported(
                "/expected/longhand_version",
                UnsupportedReason::LonghandVersion,
            ));
        }
        if self.name.is_empty() {
            return Err(invalid("/name", InvalidReason::EmptyName));
        }
        if !self.margin.is_finite() || self.margin <= 0.0 {
            return Err(invalid("/margin", InvalidReason::Margin));
        }
        if self.documents.is_empty() {
            return Err(invalid("/documents", InvalidReason::EmptyDocuments));
        }
        let mut ids = BTreeSet::new();
        for (i, doc) in self.documents.iter().enumerate() {
            if doc.id.is_empty() || !ids.insert(doc.id.as_str()) {
                return Err(invalid(
                    &format!("/documents/{i}/id"),
                    InvalidReason::DocumentId,
                ));
            }
        }
        let unique_known = |list: &[String]| {
            let unique: BTreeSet<_> = list.iter().map(String::as_str).collect();
            unique.len() == list.len() && unique.is_subset(&ids)
        };
        if let Filter::Include { ids } | Filter::Exclude { ids } = &self.filter {
            if !unique_known(ids) {
                return Err(invalid("/filter/ids", InvalidReason::FilterIds));
            }
        }
        if !unique_known(&self.expected.ids)
            || self.expected.ids.windows(2).any(|ids| ids[0] >= ids[1])
        {
            return Err(invalid("/expected/ids", InvalidReason::ExpectedIds));
        }
        self.query.validate(&Location::Query {
            path: String::new(),
        })?;
        for doc in &self.documents {
            doc.geometry.validate(&Location::Document {
                id: doc.id.clone(),
                path: String::new(),
            })?;
        }
        if self.margin < MIN_MARGIN {
            return Err(unsupported("/margin", UnsupportedReason::MarginFloor));
        }
        if self.documents.len() > 1024 {
            return Err(unsupported("/documents", UnsupportedReason::Size));
        }
        Ok(())
    }

    pub fn validate_supported(&self) -> Result<(), Rejection> {
        let check = |geometry: &Geometry, at: Location| {
            geometry.supported(&at)?;
            if self.predicate == Predicate::CoveredBy
                && !matches!(
                    geometry,
                    Geometry::Polygon { .. } | Geometry::MultiPolygon { .. }
                )
            {
                return Err(Rejection::unsupported(
                    at,
                    Some(geometry.family()),
                    UnsupportedReason::Family,
                ));
            }
            Ok(())
        };
        check(
            &self.query,
            Location::Query {
                path: String::new(),
            },
        )?;
        for doc in &self.documents {
            check(
                &doc.geometry,
                Location::Document {
                    id: doc.id.clone(),
                    path: String::new(),
                },
            )?;
        }
        Ok(())
    }
}

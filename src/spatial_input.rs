use serde_json::Value;
use tantivy::schema::document::{ErasedValue, ReferenceValueLeaf};
use tantivy::schema::{FieldEntry, FieldType};
use tantivy::spatial::SpatialInput;
use tantivy::{DocId, TantivyError};
use tantivy_spatial::geometry_set::GeometrySet;
use tantivy_spatial::surface::Surface;

const TYPE_NAME: &str = "spatial";

pub struct OpaqueGeoJson;

impl SpatialInput for OpaqueGeoJson {
    fn index_name(entry: &FieldEntry) -> tantivy::Result<Option<&str>> {
        let FieldType::Custom(options) = entry.field_type() else {
            return Ok(None);
        };
        if options.type_name() != TYPE_NAME {
            return Ok(None);
        }
        options
            .params()
            .get("index")
            .and_then(Value::as_str)
            .map(Some)
            .ok_or_else(|| {
                TantivyError::InvalidArgument(format!(
                    "custom spatial field '{}' has no string index parameter",
                    entry.name()
                ))
            })
    }

    fn convert<S: Surface>(
        doc_id: DocId,
        value: ErasedValue<'_>,
    ) -> tantivy::Result<Option<GeometrySet<S>>> {
        let ErasedValue::Leaf(ReferenceValueLeaf::Custom(bytes)) = value else {
            return Ok(None);
        };
        let geometry: Value = serde_json::from_slice(bytes).map_err(|error| {
            TantivyError::InvalidArgument(format!("invalid GeoJSON geometry: {error}"))
        })?;
        let mut set = GeometrySet::new(doc_id);
        append_geometry::<S>(&geometry, &mut set).map_err(TantivyError::InvalidArgument)?;
        Ok(Some(set))
    }
}

fn append_geometry<S: Surface>(value: &Value, set: &mut GeometrySet<S>) -> Result<(), String> {
    let object = value
        .as_object()
        .ok_or_else(|| "GeoJSON geometry is not an object".to_string())?;
    let geometry_type = object
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| "GeoJSON geometry has no type".to_string())?;

    match geometry_type {
        "Point" => set.push_point(point::<S>(coordinates(object)?)?),
        "MultiPoint" => {
            for value in array(coordinates(object)?)? {
                set.push_point(point::<S>(value)?);
            }
        }
        "LineString" => set.push_line_string(line::<S>(coordinates(object)?)?),
        "MultiLineString" => {
            for value in array(coordinates(object)?)? {
                set.push_line_string(line::<S>(value)?);
            }
        }
        "Polygon" => set.push_polygon(&polygon::<S>(coordinates(object)?)?),
        "MultiPolygon" => {
            for value in array(coordinates(object)?)? {
                set.push_polygon(&polygon::<S>(value)?);
            }
        }
        "GeometryCollection" => {
            let geometries = object
                .get("geometries")
                .ok_or_else(|| "GeoJSON collection has no geometries".to_string())?;
            for geometry in array(geometries)? {
                append_geometry::<S>(geometry, set)?;
            }
        }
        other => return Err(format!("unsupported GeoJSON geometry type '{other}'")),
    }
    Ok(())
}

fn coordinates<'a>(object: &'a serde_json::Map<String, Value>) -> Result<&'a Value, String> {
    object
        .get("coordinates")
        .ok_or_else(|| "GeoJSON geometry has no coordinates".to_string())
}

fn array(value: &Value) -> Result<&[Value], String> {
    value
        .as_array()
        .map(Vec::as_slice)
        .ok_or_else(|| "GeoJSON coordinates are not an array".to_string())
}

fn point<S: Surface>(value: &Value) -> Result<S::Point, String> {
    let coordinates = array(value)?;
    if coordinates.len() < 2 {
        return Err("GeoJSON position has fewer than two coordinates".to_string());
    }
    let longitude = coordinates[0]
        .as_f64()
        .ok_or_else(|| "GeoJSON longitude is not a number".to_string())?;
    let latitude = coordinates[1]
        .as_f64()
        .ok_or_else(|| "GeoJSON latitude is not a number".to_string())?;
    if !longitude.is_finite() || !latitude.is_finite() {
        return Err("GeoJSON position is not finite".to_string());
    }
    Ok(S::project(longitude, latitude))
}

fn line<S: Surface>(value: &Value) -> Result<Vec<S::Point>, String> {
    array(value)?.iter().map(point::<S>).collect()
}

fn polygon<S: Surface>(value: &Value) -> Result<Vec<Vec<S::Point>>, String> {
    array(value)?.iter().map(line::<S>).collect()
}

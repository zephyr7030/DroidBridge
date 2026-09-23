//! The one reading of what an Android visual primitive answers. Both hosts speak the same wire
//! shapes to the same platform code, so the decoding and the validation live here once; each host
//! keeps only its own transport and its own temporary files.

use crate::VisualSceneProof;
use crate::{VISUAL_MAX_IMAGE_BYTES, VisualDisplaySnapshot, VisualHierarchySnapshot};
use contract::{DisplayGeometry, ErrorCode, ForegroundFact, ImageFormat, VisualNode};
use domain::DomainError;
use serde::Deserialize;

/// The largest width or height a captured or transformed image may report.
const MAX_IMAGE_EDGE: u32 = 16_384;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DisplayWire {
    display: DisplayGeometry,
    display_generation: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AccessibilityHierarchyWire {
    display: DisplayGeometry,
    display_generation: u64,
    #[serde(default)]
    foreground: Option<ForegroundFact>,
    nodes: Vec<VisualNode>,
    truncated: bool,
    component_generation: u64,
    window_id: i32,
    scene_revision: u64,
    hierarchy_sha256: String,
}

/// What an encoded image answers about itself; its bytes arrive through a descriptor.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EncodedImageWire {
    pub format: ImageFormat,
    pub mime: String,
    pub size: u64,
    pub width: u32,
    pub height: u32,
    #[serde(default)]
    pub display: Option<DisplayGeometry>,
    #[serde(default)]
    pub display_generation: Option<u64>,
}

pub fn decode_display(payload: &[u8]) -> Result<VisualDisplaySnapshot, DomainError> {
    display(
        serde_json::from_slice(payload).map_err(|_| invalid("visual display result is invalid"))?,
    )
}

pub fn decode_display_value(
    payload: serde_json::Value,
) -> Result<VisualDisplaySnapshot, DomainError> {
    display(
        serde_json::from_value(payload).map_err(|_| invalid("visual display result is invalid"))?,
    )
}

pub fn decode_accessibility_hierarchy(
    payload: &[u8],
) -> Result<VisualHierarchySnapshot, DomainError> {
    hierarchy(
        serde_json::from_slice(payload)
            .map_err(|_| invalid("Accessibility hierarchy result is invalid"))?,
    )
}

pub fn decode_accessibility_hierarchy_value(
    payload: serde_json::Value,
) -> Result<VisualHierarchySnapshot, DomainError> {
    hierarchy(
        serde_json::from_value(payload)
            .map_err(|_| invalid("Accessibility hierarchy result is invalid"))?,
    )
}

pub fn decode_encoded_image(payload: &[u8]) -> Result<EncodedImageWire, DomainError> {
    let wire: EncodedImageWire =
        serde_json::from_slice(payload).map_err(|_| invalid("visual image result is invalid"))?;
    validate_image_metadata(&wire)?;
    Ok(wire)
}

pub fn decode_encoded_image_value(
    payload: serde_json::Value,
) -> Result<EncodedImageWire, DomainError> {
    let wire: EncodedImageWire =
        serde_json::from_value(payload).map_err(|_| invalid("visual image result is invalid"))?;
    validate_image_metadata(&wire)?;
    Ok(wire)
}

/// The interaction proof of an observation taken through Accessibility; any other proof cannot
/// address that scene.
pub fn accessibility_proof(proof: &VisualSceneProof) -> Option<serde_json::Value> {
    match proof {
        VisualSceneProof::Accessibility {
            component_generation,
            window_id,
            scene_revision,
            hierarchy_sha256,
        } => Some(serde_json::json!({
            "component_generation": component_generation,
            "window_id": window_id,
            "scene_revision": scene_revision,
            "hierarchy_sha256": hierarchy_sha256,
        })),
        _ => None,
    }
}

/// The bytes answer for themselves: the format's own header, and for PNG its own dimensions.
pub fn validate_encoded_bytes(format: ImageFormat, bytes: &[u8]) -> Result<(), DomainError> {
    match format {
        ImageFormat::Png => png_dimensions(bytes).map(|_| ()),
        ImageFormat::Heic
            if bytes.len() >= 12
                && &bytes[4..8] == b"ftyp"
                && matches!(
                    &bytes[8..12],
                    b"heic" | b"heix" | b"hevc" | b"hevx" | b"mif1"
                ) =>
        {
            Ok(())
        }
        ImageFormat::Heic => Err(invalid("HEIC image bytes are invalid")),
        ImageFormat::Jpeg if bytes.starts_with(&[0xff, 0xd8]) && bytes.ends_with(&[0xff, 0xd9]) => {
            Ok(())
        }
        ImageFormat::Jpeg => Err(invalid("JPEG image bytes are invalid")),
    }
}

pub fn png_dimensions(bytes: &[u8]) -> Result<(u32, u32), DomainError> {
    if bytes.len() < 24
        || &bytes[..8] != b"\x89PNG\r\n\x1a\n"
        || &bytes[8..12] != 13u32.to_be_bytes().as_slice()
        || &bytes[12..16] != b"IHDR"
    {
        return Err(invalid("PNG image bytes are invalid"));
    }
    let width = u32::from_be_bytes(bytes[16..20].try_into().expect("four header bytes"));
    let height = u32::from_be_bytes(bytes[20..24].try_into().expect("four header bytes"));
    if width == 0 || height == 0 {
        return Err(invalid("PNG dimensions are invalid"));
    }
    Ok((width, height))
}

fn validate_image_metadata(wire: &EncodedImageWire) -> Result<(), DomainError> {
    let expected_mime = match wire.format {
        ImageFormat::Heic => "image/heic",
        ImageFormat::Jpeg => "image/jpeg",
        ImageFormat::Png => "image/png",
    };
    if wire.mime != expected_mime
        || wire.size == 0
        || wire.size > VISUAL_MAX_IMAGE_BYTES as u64
        || wire.width == 0
        || wire.height == 0
        || wire.width > MAX_IMAGE_EDGE
        || wire.height > MAX_IMAGE_EDGE
    {
        return Err(invalid("visual image metadata is invalid"));
    }
    Ok(())
}

fn display(wire: DisplayWire) -> Result<VisualDisplaySnapshot, DomainError> {
    Ok(VisualDisplaySnapshot {
        display: wire.display,
        display_generation: wire.display_generation,
    })
}

fn hierarchy(wire: AccessibilityHierarchyWire) -> Result<VisualHierarchySnapshot, DomainError> {
    Ok(VisualHierarchySnapshot {
        display: VisualDisplaySnapshot {
            display: wire.display,
            display_generation: wire.display_generation,
        },
        foreground: wire.foreground,
        nodes: wire.nodes,
        truncated: wire.truncated,
        proof: VisualSceneProof::Accessibility {
            component_generation: wire.component_generation,
            window_id: wire.window_id,
            scene_revision: wire.scene_revision,
            hierarchy_sha256: wire.hierarchy_sha256,
        },
    })
}

fn invalid(reason: &'static str) -> DomainError {
    DomainError::new(ErrorCode::IoError, reason)
}

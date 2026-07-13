use crate::app::auth::RequestSurface;
use axum::{Json, extract::Extension};
use serde::Serialize;
use serde_json::Value;

pub(crate) const REMOTE_GRID_ARTWORK_SIZE: u32 = 600;
pub(crate) const REMOTE_DETAIL_ARTWORK_SIZE: u32 = 600;
pub(crate) const REMOTE_LIBRARY_ARTWORK_SIZE: u32 = 600;
pub(crate) const REMOTE_NOW_PLAYING_ARTWORK_SIZE: u32 = 600;

pub(crate) fn is_remote(surface: &Option<Extension<RequestSurface>>) -> bool {
    matches!(surface, Some(Extension(RequestSurface::Remote)))
}

pub(crate) fn artwork_json<T: Serialize>(
    value: T,
    surface: Option<Extension<RequestSurface>>,
    size: u32,
) -> Result<Json<Value>, String> {
    let mut value = serde_json::to_value(value).map_err(|e| format!("serialize response: {e}"))?;
    if is_remote(&surface) {
        resize_qobuz_image_urls(&mut value, size);
    }
    Ok(Json(value))
}

fn resize_qobuz_image_urls(value: &mut Value, size: u32) {
    match value {
        Value::Object(object) => {
            for (key, child) in object {
                if key == "image_url" {
                    if let Some(url) = child.as_str() {
                        *child = Value::String(crate::services::qobuz::sized_cover_url(url, size));
                    }
                } else {
                    resize_qobuz_image_urls(child, size);
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                resize_qobuz_image_urls(item, size);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::Extension;
    use serde_json::json;

    #[test]
    fn remote_artwork_json_downsizes_nested_qobuz_covers() {
        let response = artwork_json(
            json!({
                "album": {
                    "image_url": "https://static.qobuz.com/images/covers/ab/cd/cover_org.jpg"
                },
                "tracks": [
                    {
                        "image_url": "https://static.qobuz.com/images/covers/ab/cd/cover_4000.jpg"
                    }
                ]
            }),
            Some(Extension(RequestSurface::Remote)),
            REMOTE_GRID_ARTWORK_SIZE,
        )
        .unwrap()
        .0;

        assert_eq!(
            response.pointer("/album/image_url").and_then(Value::as_str),
            Some("https://static.qobuz.com/images/covers/ab/cd/cover_600.jpg")
        );
        assert_eq!(
            response
                .pointer("/tracks/0/image_url")
                .and_then(Value::as_str),
            Some("https://static.qobuz.com/images/covers/ab/cd/cover_600.jpg")
        );
    }

    #[test]
    fn local_artwork_json_keeps_cover_urls_unchanged() {
        let response = artwork_json(
            json!({
                "image_url": "https://static.qobuz.com/images/covers/ab/cd/cover_org.jpg"
            }),
            None,
            REMOTE_GRID_ARTWORK_SIZE,
        )
        .unwrap()
        .0;

        assert_eq!(
            response.get("image_url").and_then(Value::as_str),
            Some("https://static.qobuz.com/images/covers/ab/cd/cover_org.jpg")
        );
    }
}

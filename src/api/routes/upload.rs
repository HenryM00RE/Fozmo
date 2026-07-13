use super::internal_error;
use crate::app::state::AppState;
use axum::{
    Router,
    extract::{DefaultBodyLimit, Multipart, State},
    http::StatusCode,
    routing::post,
};
use std::fs;
use std::path::Path as StdPath;

const MAX_UPLOAD_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MAX_UPLOAD_REQUEST_BYTES: usize = (MAX_UPLOAD_BYTES as usize) + (16 * 1024 * 1024);

pub fn routes() -> Router<AppState> {
    Router::new().route(
        "/api/upload",
        post(upload_file).layer(DefaultBodyLimit::max(MAX_UPLOAD_REQUEST_BYTES)),
    )
}

async fn upload_file(
    State(state): State<AppState>,
    mut multipart: Multipart,
) -> Result<StatusCode, (StatusCode, String)> {
    let music_dir = state.music_dir();
    if !music_dir.exists() {
        fs::create_dir_all(music_dir)
            .map_err(|e| internal_error(format!("Failed to create music directory: {e:?}")))?;
    }

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|e| (e.status(), format!("Multipart error: {e:?}")))?
    {
        if let Some(filename) = field.file_name() {
            let filename = sanitized_upload_filename(filename)?;

            let save_path = music_dir.join(&filename);
            let mut file = tokio::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&save_path)
                .await
                .map_err(|e| {
                    if e.kind() == std::io::ErrorKind::AlreadyExists {
                        (
                            StatusCode::CONFLICT,
                            format!("A file named '{filename}' already exists"),
                        )
                    } else {
                        internal_error(format!("Failed to create file: {e:?}"))
                    }
                })?;
            let mut written = 0_u64;
            let mut field = field;
            while let Some(chunk) = match field.chunk().await {
                Ok(chunk) => chunk,
                Err(e) => {
                    drop(file);
                    let _ = tokio::fs::remove_file(&save_path).await;
                    return Err(internal_error(format!("Failed to read file chunk: {e:?}")));
                }
            } {
                written = written.saturating_add(chunk.len() as u64);
                if written > MAX_UPLOAD_BYTES {
                    drop(file);
                    let _ = tokio::fs::remove_file(&save_path).await;
                    return Err((
                        StatusCode::PAYLOAD_TOO_LARGE,
                        "Uploaded file is too large".to_string(),
                    ));
                }
                use tokio::io::AsyncWriteExt;
                if let Err(e) = file.write_all(&chunk).await {
                    drop(file);
                    let _ = tokio::fs::remove_file(&save_path).await;
                    return Err(internal_error(format!("Failed to save file: {e:?}")));
                }
            }
        }
    }

    Ok(StatusCode::CREATED)
}

fn sanitized_upload_filename(filename: &str) -> Result<String, (StatusCode, String)> {
    let filename = StdPath::new(filename)
        .file_name()
        .and_then(|file| file.to_str())
        .filter(|file| {
            !file.is_empty() && *file != "." && *file != ".." && !file.contains(['/', '\\', '\0'])
        })
        .ok_or((StatusCode::BAD_REQUEST, "Invalid filename".to_string()))?;
    Ok(filename.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn property_sanitized_names_remain_beneath_the_upload_directory(filename in ".{0,256}") {
            if let Ok(filename) = sanitized_upload_filename(&filename) {
                prop_assert!(!filename.contains(['/', '\\', '\0']));
                prop_assert_ne!(filename.as_str(), ".");
                prop_assert_ne!(filename.as_str(), "..");
                let root = StdPath::new("/music");
                let joined = root.join(filename);
                prop_assert_eq!(joined.parent(), Some(root));
            }
        }
    }
}

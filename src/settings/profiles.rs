use super::{DEFAULT_PROFILE_ID, ListeningProfile, PersistedSettings};
use crate::settings::store::SettingsStore;
use base64::{Engine as _, engine::general_purpose::STANDARD};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const MAX_RECENT_SEARCHES: usize = 10;
const MAX_RECENT_SEARCH_LENGTH: usize = 160;

const PROFILE_COLORS: [&str; 8] = [
    "#4f84a5", "#59806c", "#7c8f6a", "#b08a3c", "#b76450", "#9a6b9b", "#6372a0", "#8a6f50",
];

impl PersistedSettings {
    pub fn normalized_profiles(&self) -> Vec<ListeningProfile> {
        let mut profiles = self.profiles.clone().unwrap_or_default();
        profiles.retain(|profile| !profile.id.trim().is_empty() && !profile.name.trim().is_empty());
        for profile in &mut profiles {
            profile.recent_searches = normalized_recent_searches(&profile.recent_searches);
        }
        if profiles.is_empty() {
            profiles.push(ListeningProfile::default());
        }
        profiles
    }

    pub fn active_profile_id(&self) -> String {
        let profiles = self.normalized_profiles();
        if let Some(active) = self.active_profile_id.as_deref()
            && profiles.iter().any(|profile| profile.id == active)
        {
            return active.to_string();
        }
        profiles
            .first()
            .map(|profile| profile.id.clone())
            .unwrap_or_else(|| DEFAULT_PROFILE_ID.to_string())
    }

    pub(super) fn normalize_profiles(&mut self) {
        let profiles = self.normalized_profiles();
        let active = self
            .active_profile_id
            .clone()
            .filter(|active| profiles.iter().any(|profile| &profile.id == active))
            .unwrap_or_else(|| profiles[0].id.clone());
        self.profiles = Some(profiles);
        self.active_profile_id = Some(active);
    }
}

impl SettingsStore {
    pub fn create_profile(&self, name: &str) -> Result<ListeningProfile, String> {
        let name = name.trim();
        if name.is_empty() {
            return Err("Profile name is required".to_string());
        }
        self.commit_mutation(|next| {
            next.normalize_profiles();
            let mut profiles = next.profiles.clone().unwrap_or_default();
            if profiles
                .iter()
                .any(|profile| profile.name.eq_ignore_ascii_case(name))
            {
                return Err("A profile with that name already exists".to_string());
            }
            let profile = ListeningProfile {
                id: unique_profile_id(name, &profiles),
                name: name.to_string(),
                color: profile_color(name, profiles.len()),
                image: None,
                recent_searches: Vec::new(),
            };
            profiles.push(profile.clone());
            next.profiles = Some(profiles);
            next.active_profile_id = Some(profile.id.clone());
            Ok(profile)
        })
    }

    pub fn select_profile(&self, profile_id: &str) -> Result<ListeningProfile, String> {
        self.commit_mutation(|next| {
            next.normalize_profiles();
            let profiles = next.profiles.clone().unwrap_or_default();
            let Some(profile) = profiles
                .iter()
                .find(|profile| profile.id == profile_id)
                .cloned()
            else {
                return Err("Profile not found".to_string());
            };
            next.active_profile_id = Some(profile.id.clone());
            Ok(profile)
        })
    }

    pub fn update_profile(
        &self,
        profile_id: &str,
        name: &str,
        color: &str,
        image: Option<&str>,
    ) -> Result<ListeningProfile, String> {
        let name = name.trim();
        if name.is_empty() {
            return Err("Profile name is required".to_string());
        }
        let color = normalized_profile_color(color)?;
        let existing_image = self
            .profiles()
            .into_iter()
            .find(|profile| profile.id == profile_id)
            .and_then(|profile| profile.image);
        let requested_image = image.map(str::trim).filter(|image| !image.is_empty());
        let (prepared_image, model_image) = match requested_image {
            Some(image) if Some(image) == existing_image.as_deref() => {
                (None, existing_image.clone())
            }
            Some(image) if image.starts_with("/profile-images/") => {
                return Err("Profile image identifier is not valid for this profile".to_string());
            }
            _ => {
                let prepared = prepare_profile_image(&self.path, profile_id, requested_image)?;
                let model = prepared.as_ref().map(|prepared| prepared.url.clone());
                (prepared, model)
            }
        };
        let result = self.commit_mutation(|next| {
            next.normalize_profiles();
            let mut profiles = next.profiles.clone().unwrap_or_default();
            if profiles
                .iter()
                .any(|profile| profile.id != profile_id && profile.name.eq_ignore_ascii_case(name))
            {
                return Err("A profile with that name already exists".to_string());
            }
            let Some(profile) = profiles.iter_mut().find(|profile| profile.id == profile_id) else {
                return Err("Profile not found".to_string());
            };
            let previous_image = profile.image.clone();
            profile.name = name.to_string();
            profile.color = color.to_string();
            profile.image = model_image;
            let profile = profile.clone();
            next.profiles = Some(profiles);
            Ok((profile, previous_image))
        });
        match result {
            Ok((profile, previous_image)) => {
                remove_replaced_profile_image(
                    &self.path,
                    previous_image.as_deref(),
                    profile.image.as_deref(),
                );
                Ok(profile)
            }
            Err(error) => {
                if let Some(prepared) = prepared_image {
                    let _ = std::fs::remove_file(prepared.path);
                }
                Err(error)
            }
        }
    }

    pub fn update_profile_recent_searches(
        &self,
        profile_id: &str,
        searches: &[String],
    ) -> Result<Vec<String>, String> {
        self.commit_mutation(|next| {
            next.normalize_profiles();
            let mut profiles = next.profiles.clone().unwrap_or_default();
            let Some(profile) = profiles.iter_mut().find(|profile| profile.id == profile_id) else {
                return Err("Profile not found".to_string());
            };
            profile.recent_searches = normalized_recent_searches(searches);
            let recent_searches = profile.recent_searches.clone();
            next.profiles = Some(profiles);
            Ok(recent_searches)
        })
    }

    pub fn delete_profile(&self, profile_id: &str) -> Result<String, String> {
        let (active, removed_image) = self.commit_mutation(|next| {
            next.normalize_profiles();
            let mut profiles = next.profiles.clone().unwrap_or_default();
            if profiles.len() <= 1 {
                return Err("At least one profile is required".to_string());
            }
            let Some(index) = profiles.iter().position(|profile| profile.id == profile_id) else {
                return Err("Profile not found".to_string());
            };
            let removed_image = profiles.remove(index).image;
            let active = next
                .active_profile_id
                .clone()
                .filter(|active| {
                    active != profile_id && profiles.iter().any(|profile| &profile.id == active)
                })
                .unwrap_or_else(|| profiles[0].id.clone());
            next.profiles = Some(profiles);
            next.active_profile_id = Some(active.clone());
            Ok((active, removed_image))
        })?;
        remove_replaced_profile_image(&self.path, removed_image.as_deref(), None);
        Ok(active)
    }
}

fn normalized_recent_searches(searches: &[String]) -> Vec<String> {
    let mut normalized = Vec::new();
    for search in searches {
        let trimmed = search.trim();
        if trimmed.is_empty() {
            continue;
        }
        let truncated = trimmed
            .chars()
            .take(MAX_RECENT_SEARCH_LENGTH)
            .collect::<String>();
        if normalized
            .iter()
            .any(|existing: &String| existing.eq_ignore_ascii_case(&truncated))
        {
            continue;
        }
        normalized.push(truncated);
        if normalized.len() >= MAX_RECENT_SEARCHES {
            break;
        }
    }
    normalized
}

fn unique_profile_id(name: &str, profiles: &[ListeningProfile]) -> String {
    let slug = slugify_profile_name(name);
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default();
    let base = format!("{slug}-{millis:x}");
    if !profiles.iter().any(|profile| profile.id == base) {
        return base;
    }
    let mut idx = 2;
    loop {
        let candidate = format!("{base}-{idx}");
        if !profiles.iter().any(|profile| profile.id == candidate) {
            return candidate;
        }
        idx += 1;
    }
}

fn slugify_profile_name(name: &str) -> String {
    let slug = name
        .chars()
        .filter_map(|ch| {
            if ch.is_ascii_alphanumeric() {
                Some(ch.to_ascii_lowercase())
            } else if ch.is_whitespace() || matches!(ch, '-' | '_') {
                Some('-')
            } else {
                None
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    if slug.is_empty() {
        "profile".to_string()
    } else {
        slug
    }
}

fn profile_color(name: &str, offset: usize) -> String {
    let hash = name.bytes().fold(offset, |acc, byte| {
        acc.wrapping_mul(31).wrapping_add(byte as usize)
    });
    PROFILE_COLORS[hash % PROFILE_COLORS.len()].to_string()
}

fn normalized_profile_color(color: &str) -> Result<&'static str, String> {
    PROFILE_COLORS
        .iter()
        .copied()
        .find(|candidate| candidate.eq_ignore_ascii_case(color.trim()))
        .ok_or_else(|| "Choose one of the available profile colors".to_string())
}

struct PreparedProfileImage {
    url: String,
    path: PathBuf,
}

fn prepare_profile_image(
    settings_path: &Path,
    profile_id: &str,
    image: Option<&str>,
) -> Result<Option<PreparedProfileImage>, String> {
    let Some(image) = image.map(str::trim) else {
        return Ok(None);
    };
    if image.is_empty() {
        return Ok(None);
    }
    if image.len() > 1_000_000 {
        return Err("Profile image is too large".to_string());
    }
    let (declared_mime, payload) = image
        .split_once(";base64,")
        .ok_or_else(|| "Profile image must be a JPEG, PNG, or WebP image".to_string())?;
    let declared_mime = declared_mime
        .strip_prefix("data:")
        .ok_or_else(|| "Profile image must be a JPEG, PNG, or WebP image".to_string())?;
    let bytes = STANDARD
        .decode(payload)
        .map_err(|_| "Profile image data is not valid base64".to_string())?;
    if bytes.len() > 750_000 {
        return Err("Profile image is too large".to_string());
    }
    let format = image::guess_format(&bytes)
        .map_err(|_| "Profile image contents are invalid".to_string())?;
    let (mime, extension) = match format {
        image::ImageFormat::Jpeg => ("image/jpeg", "jpg"),
        image::ImageFormat::Png => ("image/png", "png"),
        image::ImageFormat::WebP => ("image/webp", "webp"),
        _ => return Err("Profile image must be a JPEG, PNG, or WebP image".to_string()),
    };
    if declared_mime != mime {
        return Err("Profile image type does not match its contents".to_string());
    }
    let dimensions = imagesize::blob_size(&bytes)
        .map_err(|_| "Profile image contents are invalid".to_string())?;
    if dimensions.width == 0
        || dimensions.height == 0
        || dimensions.width > 4096
        || dimensions.height > 4096
    {
        return Err("Profile image dimensions are invalid".to_string());
    }

    let profile_hash = Sha256::digest(profile_id.as_bytes());
    let content_hash = Sha256::digest(&bytes);
    let filename = format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}.{extension}",
        profile_hash[0],
        profile_hash[1],
        profile_hash[2],
        profile_hash[3],
        content_hash[0],
        content_hash[1],
        content_hash[2],
        content_hash[3],
    );
    let directory = profile_images_dir(settings_path);
    std::fs::create_dir_all(&directory)
        .map_err(|error| format!("create profile image directory: {error}"))?;
    let path = directory.join(&filename);
    crate::app::paths::atomic_write(&path, &bytes)?;
    Ok(Some(PreparedProfileImage {
        url: format!("/profile-images/{filename}"),
        path,
    }))
}

pub(super) fn migrate_profile_images(
    settings_path: &Path,
    settings: &mut PersistedSettings,
) -> Result<bool, String> {
    let mut changed = false;
    let mut profiles = settings.normalized_profiles();
    for profile in &mut profiles {
        if profile
            .image
            .as_deref()
            .is_some_and(|image| image.starts_with("data:image/"))
        {
            let prepared =
                prepare_profile_image(settings_path, &profile.id, profile.image.as_deref())?;
            profile.image = prepared.map(|image| image.url);
            changed = true;
        }
    }
    if changed {
        settings.profiles = Some(profiles);
        settings.normalize_profiles();
    }
    Ok(changed)
}

fn profile_images_dir(settings_path: &Path) -> PathBuf {
    settings_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("profile-images")
}

fn remove_replaced_profile_image(
    settings_path: &Path,
    previous: Option<&str>,
    current: Option<&str>,
) {
    let Some(previous) = previous else { return };
    if Some(previous) == current {
        return;
    }
    let Some(filename) = previous.strip_prefix("/profile-images/") else {
        return;
    };
    if Path::new(filename)
        .file_name()
        .and_then(|name| name.to_str())
        != Some(filename)
    {
        return;
    }
    let _ = std::fs::remove_file(profile_images_dir(settings_path).join(filename));
}

#[cfg(test)]
mod tests {
    use super::normalized_recent_searches;

    #[test]
    fn recent_searches_are_normalized_and_limited_to_ten() {
        let searches = vec![
            "  First  ".to_string(),
            "SECOND".to_string(),
            "first".to_string(),
            "Third".to_string(),
            "Fourth".to_string(),
            "Fifth".to_string(),
            "Sixth".to_string(),
            "Seventh".to_string(),
            "Eighth".to_string(),
            "Ninth".to_string(),
            "Tenth".to_string(),
            "Eleventh".to_string(),
        ];

        let normalized = normalized_recent_searches(&searches);

        assert_eq!(normalized.len(), 10);
        assert_eq!(normalized[0], "First");
        assert_eq!(normalized[1], "SECOND");
        assert_eq!(normalized[9], "Tenth");
        assert!(!normalized.iter().any(|search| search == "Eleventh"));
    }
}

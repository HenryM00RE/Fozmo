import { type CSSProperties, useEffect, useRef, useState } from 'react';
import type { JsonRecord } from '../../../shared/types';
import { Icon } from '../../../shared/ui/Icon';
import { Modal } from '../../../shared/ui/Modal';
import { SelectMenu } from '../../../shared/ui/SelectMenu';
import { PROFILE_COLORS } from '../settingsModel';

const SETTINGS_ICON_PATH =
  'M9.67 4.14a2.34 2.34 0 0 1 4.66 0 2.34 2.34 0 0 0 3.32 1.91 2.34 2.34 0 0 1 2.33 4.03 2.34 2.34 0 0 0 0 3.84 2.34 2.34 0 0 1-2.33 4.03 2.34 2.34 0 0 0-3.32 1.91 2.34 2.34 0 0 1-4.66 0 2.34 2.34 0 0 0-3.32-1.91 2.34 2.34 0 0 1-2.33-4.03 2.34 2.34 0 0 0 0-3.84 2.34 2.34 0 0 1 2.33-4.03 2.34 2.34 0 0 0 3.32-1.91ZM12 9a3 3 0 1 0 0 6 3 3 0 0 0 0-6Z';
const profileColorOptions = PROFILE_COLORS.map((color) => ({
  value: color.value,
  label: color.label,
  color: color.value
}));

export function ProfilesSettingsPage({
  activeProfileId,
  createProfile,
  deleteProfile,
  profileName,
  profiles,
  selectProfile,
  setProfileName,
  updateProfile
}: {
  activeProfileId: string;
  createProfile: () => Promise<void>;
  deleteProfile: (profileId: string) => Promise<void>;
  profileName: string;
  profiles: JsonRecord[];
  selectProfile: (profileId: string) => Promise<void>;
  setProfileName: (value: string) => void;
  updateProfile: (
    profileId: string,
    name: string,
    color: string,
    image?: string | null
  ) => Promise<void>;
}) {
  const [editingProfile, setEditingProfile] = useState<JsonRecord | null>(null);
  const [draftName, setDraftName] = useState('');
  const [draftColor, setDraftColor] = useState<string>(PROFILE_COLORS[0].value);
  const [draftImage, setDraftImage] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [imageBusy, setImageBusy] = useState(false);
  const [error, setError] = useState('');
  const [confirmDelete, setConfirmDelete] = useState(false);
  const imageUploadRef = useRef<Promise<string> | null>(null);

  useEffect(() => {
    if (!editingProfile) return;
    setDraftName(String(editingProfile.name || editingProfile.id || ''));
    setDraftColor(profileColor(editingProfile));
    setDraftImage(profileImage(editingProfile));
    imageUploadRef.current = null;
    setImageBusy(false);
    setConfirmDelete(false);
    setError('');
  }, [editingProfile]);

  const closeEditor = () => {
    if (busy) return;
    setEditingProfile(null);
    setConfirmDelete(false);
    setError('');
  };

  const saveProfile = async () => {
    if (!editingProfile) return;
    const profileId = String(editingProfile.id || '');
    if (!profileId || !draftName.trim()) return;
    setBusy(true);
    setError('');
    try {
      const image = imageUploadRef.current ? await imageUploadRef.current : draftImage;
      await updateProfile(profileId, draftName, draftColor, image);
      setEditingProfile(null);
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Could not save profile');
    } finally {
      setBusy(false);
    }
  };

  const uploadProfileImage = async (file: File | null) => {
    if (!file) return;
    setError('');
    setImageBusy(true);
    const upload = profileImageFromFile(file);
    imageUploadRef.current = upload;
    try {
      const image = await upload;
      if (imageUploadRef.current === upload) {
        setDraftImage(image);
      }
    } catch (err) {
      if (imageUploadRef.current === upload) {
        setError(err instanceof Error ? err.message : 'Could not load profile picture');
      }
    } finally {
      if (imageUploadRef.current === upload) {
        imageUploadRef.current = null;
        setImageBusy(false);
      }
    }
  };

  const removeProfile = async () => {
    if (!editingProfile) return;
    const profileId = String(editingProfile.id || '');
    if (!profileId) return;
    setBusy(true);
    setError('');
    try {
      await deleteProfile(profileId);
      setConfirmDelete(false);
      setEditingProfile(null);
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Could not delete profile');
    } finally {
      setBusy(false);
    }
  };

  return (
    <section className="settings-panel">
      <div className="settings-grid two-col">
        <section className="settings-section-block">
          <div className="settings-section-heading">
            <div className="section-label">Listening profiles</div>
          </div>
          <div className="panel raised">
            <div className="profile-settings-list">
              {profiles.map((profile) => {
                const profileId = String(profile.id || '');
                const isActive = profileId === activeProfileId;
                return (
                  <div
                    className={`profile-settings-row${isActive ? ' is-active' : ''}`}
                    key={profileId || String(profile.name)}
                  >
                    <button
                      className="profile-settings-main"
                      type="button"
                      onClick={() => selectProfile(profileId)}
                    >
                      <ProfilePicture profile={profile} sizeClass="profile-chip-settings" />
                      <span>
                        <strong>{String(profile.name || profile.id)}</strong>
                        <small>{isActive ? 'Selected profile' : 'Switch profile'}</small>
                      </span>
                    </button>
                    <button
                      className="profile-settings-edit"
                      type="button"
                      title="Profile settings"
                      aria-label={`Profile settings for ${String(profile.name || 'Profile')}`}
                      onClick={() => setEditingProfile(profile)}
                    >
                      <Icon path={SETTINGS_ICON_PATH} />
                    </button>
                  </div>
                );
              })}
            </div>
          </div>
        </section>
        <section className="settings-section-block">
          <div className="settings-section-heading">
            <div className="section-label">Create profile</div>
          </div>
          <div className="panel raised">
            <div className="settings-list">
              <div className="library-folder-row profile-create-row">
                <label className="field">
                  <span>Name</span>
                  <input
                    type="text"
                    value={profileName}
                    onChange={(event) => setProfileName(event.target.value)}
                    placeholder="Profile name"
                    maxLength={48}
                  />
                </label>
                <button className="pill primary" type="button" onClick={createProfile}>
                  Create
                </button>
              </div>
            </div>
          </div>
        </section>
      </div>

      <Modal
        open={Boolean(editingProfile)}
        className="profile-settings-backdrop"
        ariaLabelledBy="profile-settings-title"
        onClose={closeEditor}
      >
        {editingProfile ? (
          <section
            className="profile-settings-panel app-modal-surface"
            aria-labelledby="profile-settings-title"
          >
            <header className="profile-settings-head">
              <div className="profile-settings-identity">
                <ProfilePicture
                  profile={{
                    ...editingProfile,
                    name: draftName,
                    color: draftColor,
                    image: draftImage
                  }}
                  sizeClass="profile-chip-settings-large"
                />
                <div>
                  <span className="section-label">Profile settings</span>
                  <h2 id="profile-settings-title">{String(editingProfile.name || 'Profile')}</h2>
                </div>
              </div>
              <button
                className="profile-settings-close"
                type="button"
                aria-label="Close"
                onClick={closeEditor}
              >
                <Icon path="M18 6 6 18M6 6l12 12" />
              </button>
            </header>
            <div className="profile-settings-body">
              <label className="zone-settings-field">
                <span>Name</span>
                <input
                  className="zone-settings-input"
                  type="text"
                  value={draftName}
                  maxLength={48}
                  onChange={(event) => setDraftName(event.target.value)}
                />
              </label>
              <div className="profile-media-row">
                <div className="profile-picture-field">
                  <span className="section-label">Picture</span>
                  <div className="profile-picture-row">
                    <label
                      className={`profile-picture-picker${imageBusy ? ' is-busy' : ''}`}
                      aria-label="Upload profile picture"
                    >
                      <ProfilePicture
                        profile={{
                          ...editingProfile,
                          name: draftName,
                          color: draftColor,
                          image: draftImage
                        }}
                        sizeClass="profile-chip-settings-large"
                      />
                      <span className="profile-picture-upload-icon">
                        <Icon path="M12 3v12M7 8l5-5 5 5M5 21h14" />
                      </span>
                      <input
                        className="sr-only"
                        type="file"
                        accept="image/png,image/jpeg,image/webp"
                        onChange={(event) => {
                          uploadProfileImage(event.target.files?.[0] || null).catch(
                            () => undefined
                          );
                          event.currentTarget.value = '';
                        }}
                      />
                    </label>
                    {draftImage ? (
                      <button
                        className="profile-picture-clear"
                        type="button"
                        aria-label="Remove profile picture"
                        onClick={() => {
                          imageUploadRef.current = null;
                          setImageBusy(false);
                          setDraftImage(null);
                        }}
                      >
                        <Icon path="M18 6 6 18M6 6l12 12" />
                      </button>
                    ) : null}
                  </div>
                </div>
                <div className="profile-accent-field">
                  <span className="section-label">Accent</span>
                  <SelectMenu
                    className="profile-accent-select"
                    ariaLabel="Profile accent"
                    value={draftColor}
                    onChange={setDraftColor}
                    options={profileColorOptions}
                  />
                </div>
              </div>
              {error ? <div className="profile-settings-error">{error}</div> : null}
              <footer className="profile-settings-foot">
                <button
                  className="zone-settings-danger"
                  type="button"
                  onClick={() => setConfirmDelete(true)}
                  disabled={busy || profiles.length <= 1}
                >
                  Delete
                </button>
                <span className="zone-settings-spacer" />
                <button
                  className="zone-settings-pill"
                  type="button"
                  onClick={closeEditor}
                  disabled={busy}
                >
                  Close
                </button>
                <button
                  className="zone-settings-pill primary"
                  type="button"
                  onClick={saveProfile}
                  disabled={busy || imageBusy || !draftName.trim()}
                >
                  Save
                </button>
              </footer>
            </div>
          </section>
        ) : null}
      </Modal>
      <Modal
        open={Boolean(editingProfile && confirmDelete)}
        className="profile-delete-backdrop"
        ariaLabelledBy="profile-delete-title"
        onClose={() => {
          if (!busy) setConfirmDelete(false);
        }}
      >
        {editingProfile ? (
          <section
            className="profile-delete-panel app-modal-surface"
            aria-labelledby="profile-delete-title"
          >
            <header className="profile-delete-head">
              <h2 id="profile-delete-title">Delete profile?</h2>
            </header>
            <div className="profile-delete-body">
              <p>{`Delete ${String(editingProfile.name || 'this profile')}? This cannot be undone.`}</p>
            </div>
            <footer className="profile-delete-foot">
              <button
                className="zone-settings-pill"
                type="button"
                onClick={() => setConfirmDelete(false)}
                disabled={busy}
              >
                Cancel
              </button>
              <button
                className="zone-settings-danger"
                type="button"
                onClick={removeProfile}
                disabled={busy}
              >
                Delete
              </button>
            </footer>
          </section>
        ) : null}
      </Modal>
    </section>
  );
}

function ProfilePicture({ profile, sizeClass = '' }: { profile: JsonRecord; sizeClass?: string }) {
  const image = profileImage(profile);
  return (
    <span
      className={`profile-chip ${sizeClass}${image ? ' has-image' : ''}`}
      style={{ '--profile-color': profileColor(profile) } as CSSProperties}
    >
      {image ? <img src={image} alt="" /> : profileInitial(profile)}
    </span>
  );
}

function profileImage(profile: JsonRecord) {
  const image = String(profile.image || '');
  return image.startsWith('data:image/') || image.startsWith('/profile-images/') ? image : null;
}

function profileColor(profile: JsonRecord) {
  const color = String(profile.color || PROFILE_COLORS[2].value);
  return PROFILE_COLORS.some((option) => option.value.toLowerCase() === color.toLowerCase())
    ? color
    : PROFILE_COLORS[2].value;
}

function profileInitial(profile: JsonRecord) {
  return (
    String(profile.name || '?')
      .trim()
      .slice(0, 1)
      .toUpperCase() || '?'
  );
}

function profileImageFromFile(file: File) {
  if (!file.type.startsWith('image/')) return Promise.reject(new Error('Choose an image file.'));
  const maxSize = 256;
  return new Promise<string>((resolve, reject) => {
    const reader = new FileReader();
    reader.onerror = () => reject(new Error('Could not read profile picture.'));
    reader.onload = () => {
      const image = new Image();
      image.onerror = () => reject(new Error('Could not load profile picture.'));
      image.onload = () => {
        const scale = Math.min(1, maxSize / Math.max(image.width, image.height));
        const width = Math.max(1, Math.round(image.width * scale));
        const height = Math.max(1, Math.round(image.height * scale));
        const canvas = document.createElement('canvas');
        canvas.width = width;
        canvas.height = height;
        const context = canvas.getContext('2d');
        if (!context) {
          reject(new Error('Could not prepare profile picture.'));
          return;
        }
        context.drawImage(image, 0, 0, width, height);
        resolve(canvas.toDataURL('image/jpeg', 0.86));
      };
      image.src = String(reader.result || '');
    };
    reader.readAsDataURL(file);
  });
}

import { type CSSProperties, useEffect, useLayoutEffect, useRef, useState } from 'react';
import { createPortal } from 'react-dom';
import type { JsonRecord } from '../../shared/types';
import { Icon } from '../../shared/ui/Icon';
import type { ProfilesResponse } from './settingsModel';

const MOBILE_PROFILE_MENU_QUERY = '(max-width: 760px)';

type ProfileMenuProps = {
  profiles: JsonRecord[];
  activeProfileId: string;
  selectionActive: boolean;
  onExitSelection: () => void;
  onSelectProfile: (profileId: string) => Promise<ProfilesResponse>;
  onRefresh: () => Promise<void>;
  onOpenSettings: () => void;
  onNotice?: (message: string) => void;
};

export function ProfileMenu({
  profiles,
  activeProfileId,
  selectionActive,
  onExitSelection,
  onSelectProfile,
  onRefresh,
  onOpenSettings,
  onNotice
}: ProfileMenuProps) {
  const [open, setOpen] = useState(false);
  const [busyProfileId, setBusyProfileId] = useState('');
  const [portalHost, setPortalHost] = useState<Element | null>(null);
  const [portalStyle, setPortalStyle] = useState<CSSProperties | null>(null);
  const buttonRef = useRef<HTMLButtonElement | null>(null);
  const menuRef = useRef<HTMLDivElement | null>(null);
  const wrapRef = useRef<HTMLDivElement | null>(null);
  const visibleProfiles = profiles.length ? profiles : [activeProfile(profiles, activeProfileId)];
  const active = activeProfile(visibleProfiles, activeProfileId);

  useEffect(() => {
    if (!open) return;
    const onPointerDown = (event: PointerEvent) => {
      const target = event.target as Node;
      if (wrapRef.current?.contains(target) || menuRef.current?.contains(target)) return;
      setOpen(false);
    };
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape') setOpen(false);
    };
    document.addEventListener('pointerdown', onPointerDown);
    document.addEventListener('keydown', onKeyDown);
    return () => {
      document.removeEventListener('pointerdown', onPointerDown);
      document.removeEventListener('keydown', onKeyDown);
    };
  }, [open]);

  useEffect(() => {
    if (selectionActive) setOpen(false);
  }, [selectionActive]);

  useLayoutEffect(() => {
    if (!open) {
      setPortalHost(null);
      setPortalStyle(null);
      return undefined;
    }

    const updatePortalPosition = () => {
      const isMobile = window.matchMedia(MOBILE_PROFILE_MENU_QUERY).matches;
      const host = document.querySelector('.react-app');
      const rect = buttonRef.current?.getBoundingClientRect();
      if (!isMobile || !host || !rect) {
        setPortalHost(null);
        setPortalStyle(null);
        return;
      }

      setPortalHost(host);
      setPortalStyle({
        position: 'fixed',
        right: Math.max(12, window.innerWidth - rect.right),
        top: rect.bottom + 10
      });
    };

    updatePortalPosition();
    window.addEventListener('resize', updatePortalPosition);
    window.addEventListener('scroll', updatePortalPosition, true);
    window.visualViewport?.addEventListener('resize', updatePortalPosition);
    window.visualViewport?.addEventListener('scroll', updatePortalPosition);
    return () => {
      window.removeEventListener('resize', updatePortalPosition);
      window.removeEventListener('scroll', updatePortalPosition, true);
      window.visualViewport?.removeEventListener('resize', updatePortalPosition);
      window.visualViewport?.removeEventListener('scroll', updatePortalPosition);
    };
  }, [open]);

  const selectProfile = async (profile: JsonRecord) => {
    const profileId = String(profile.id || '');
    if (!profileId || profileId === activeProfileId) {
      setOpen(false);
      return;
    }
    setBusyProfileId(profileId);
    try {
      const data = await onSelectProfile(profileId);
      setOpen(false);
      const nextActive = activeProfile(
        data.profiles || visibleProfiles,
        data.active_profile_id || profileId
      );
      onNotice?.(`Profile switched to ${String(nextActive.name || 'Profile')}`);
      await onRefresh();
    } catch (error) {
      onNotice?.(error instanceof Error ? error.message : 'Could not switch profile');
    } finally {
      setBusyProfileId('');
    }
  };

  const profileMenu = open ? (
    <div
      className={`profile-menu${portalStyle ? ' is-mobile-portal' : ''}`}
      id="profile-menu"
      role="menu"
      aria-label="Profile menu"
      ref={menuRef}
      style={portalStyle || undefined}
    >
      <div className="profile-menu-head">
        <strong>Switch profile</strong>
      </div>
      <div className="profile-menu-list">
        {visibleProfiles.map((profile) => {
          const profileId = String(profile.id || '');
          const isActive = profileId === activeProfileId;
          return (
            <button
              className={`profile-menu-item profile-switch-item${isActive ? ' is-active' : ''}`}
              type="button"
              role="menuitemradio"
              aria-checked={isActive}
              disabled={busyProfileId === profileId}
              key={profileId || String(profile.name)}
              onClick={() => {
                selectProfile(profile).catch(() => undefined);
              }}
            >
              <ProfileChip profile={profile} sizeClass="profile-chip-menu" />
              <span className="profile-menu-copy">
                <strong>{String(profile.name || 'Profile')}</strong>
                <small>
                  {isActive ? 'Active' : busyProfileId === profileId ? 'Switching' : 'Switch'}
                </small>
              </span>
              {isActive ? <Icon path="m5 12 4 4L19 6" /> : null}
            </button>
          );
        })}
      </div>
      <button
        className="profile-menu-item profile-menu-settings-item"
        type="button"
        role="menuitem"
        onClick={() => {
          setOpen(false);
          onOpenSettings();
        }}
      >
        <Icon path="M9.67 4.14a2.34 2.34 0 0 1 4.66 0 2.34 2.34 0 0 0 3.32 1.91 2.34 2.34 0 0 1 2.33 4.03 2.34 2.34 0 0 0 0 3.84 2.34 2.34 0 0 1-2.33 4.03 2.34 2.34 0 0 0-3.32 1.91 2.34 2.34 0 0 1-4.66 0 2.34 2.34 0 0 0-3.32-1.91 2.34 2.34 0 0 1-2.33-4.03 2.34 2.34 0 0 0 0-3.84 2.34 2.34 0 0 1 2.33-4.03 2.34 2.34 0 0 0 3.32-1.91ZM12 9a3 3 0 1 0 0 6 3 3 0 0 0 0-6Z" />
        <span>Settings</span>
      </button>
    </div>
  ) : null;

  return (
    <div className="profile-menu-wrap" ref={wrapRef}>
      <button
        className={`profile-avatar${open ? ' is-active' : ''}${selectionActive ? ' is-selection-exit' : ''}`}
        ref={buttonRef}
        type="button"
        title={selectionActive ? 'Exit selection' : String(active.name || 'Profile')}
        aria-label={
          selectionActive ? 'Exit selection' : `Profile: ${String(active.name || 'Profile')}`
        }
        aria-haspopup={selectionActive ? undefined : 'menu'}
        aria-controls={selectionActive ? undefined : 'profile-menu'}
        aria-expanded={selectionActive ? undefined : open}
        onClick={(event) => {
          event.stopPropagation();
          if (selectionActive) {
            onExitSelection();
            return;
          }
          setOpen((current) => !current);
        }}
      >
        {selectionActive ? (
          <Icon path="M18 6 6 18M6 6l12 12" />
        ) : (
          <ProfileChip profile={active} sizeClass="profile-chip-toolbar" />
        )}
      </button>

      {profileMenu && portalHost && portalStyle
        ? createPortal(profileMenu, portalHost)
        : profileMenu}
    </div>
  );
}

function ProfileChip({ profile, sizeClass = '' }: { profile: JsonRecord; sizeClass?: string }) {
  const color = String(profile.color || '#7c8f6a');
  const image = String(profile.image || '');
  return (
    <span
      className={`profile-chip ${sizeClass}${isProfileImage(image) ? ' has-image' : ''}`}
      style={{ '--profile-color': color } as CSSProperties}
    >
      {isProfileImage(image) ? <img src={image} alt="" /> : profileInitial(profile)}
    </span>
  );
}

function isProfileImage(image: string) {
  return image.startsWith('data:image/') || image.startsWith('/profile-images/');
}

function activeProfile(profiles: JsonRecord[], activeProfileId: string) {
  return (
    profiles.find((profile) => profile.id === activeProfileId) ||
    profiles[0] || { id: 'default', name: 'Default', color: '#7c8f6a' }
  );
}

function profileInitial(profile: JsonRecord) {
  return (
    String(profile.name || '?')
      .trim()
      .slice(0, 1)
      .toUpperCase() || '?'
  );
}

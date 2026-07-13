import { useEffect, useState } from 'react';
import {
  albumVersionLabel,
  artFallback,
  idValue,
  sameVersionId,
  titleOf,
  versionArt,
  versionQualityLabel
} from '../../../shared/lib/appSupport';
import type { JsonRecord, LibraryAlbum, LibraryTrack } from '../../../shared/types';
import { Icon } from '../../../shared/ui/Icon';
import { Menu } from '../../../shared/ui/Menu';
import { QobuzSourceIcon } from '../../../shared/ui/QobuzSourceIcon';
import { useActionMenuScrollLock } from '../../../shared/ui/useActionMenuScrollLock';

const VERSION_CONTEXT_MENU_WIDTH = 230;
const VERSION_CONTEXT_MENU_EDGE_GAP = 12;

function appViewportScale() {
  if (typeof document === 'undefined') return 1;
  const app = document.querySelector('.react-app') as HTMLElement | null;
  if (!app?.offsetWidth) return 1;
  const scale = app.getBoundingClientRect().width / app.offsetWidth;
  return Number.isFinite(scale) && scale > 0 ? scale : 1;
}

function versionContextMenuPosition(clientX: number, clientY: number) {
  const scale = appViewportScale();
  const edgeGap = VERSION_CONTEXT_MENU_EDGE_GAP / scale;
  const viewportRight =
    typeof window === 'undefined'
      ? clientX / scale
      : (window.innerWidth - VERSION_CONTEXT_MENU_EDGE_GAP) / scale;
  const maxX = Math.max(edgeGap, viewportRight - VERSION_CONTEXT_MENU_WIDTH);
  return {
    x: Math.min(Math.max(edgeGap, clientX / scale), maxX),
    y: Math.max(edgeGap, clientY / scale)
  };
}

export function AlbumVersionsPanel({
  versions,
  fallbackAlbum,
  fallbackTracks,
  viewingVersionId,
  onViewVersion,
  onOpenLocalAlbum,
  onOpenQobuzAlbum,
  onSetPrimary,
  onEditLocalAlbum
}: {
  versions: JsonRecord[];
  fallbackAlbum: LibraryAlbum | null;
  fallbackTracks: LibraryTrack[];
  viewingVersionId?: string | number | null;
  onViewVersion?: (versionId: string | number) => void;
  onOpenLocalAlbum?: (id: string | number) => void;
  onOpenQobuzAlbum?: (id: string | number, albumHint?: LibraryAlbum) => void;
  onSetPrimary: (versionId: string | number) => Promise<void>;
  onEditLocalAlbum?: () => void;
}) {
  const [contextMenu, setContextMenu] = useState<{ x: number; y: number } | null>(null);
  useActionMenuScrollLock(Boolean(contextMenu));
  const ordered = versions.length
    ? [
        ...versions.filter((version) => version.provider === 'local'),
        ...versions
          .filter((version) => version.provider === 'qobuz' && version.tier !== 'catalog')
          .sort((a, b) => {
            const tierRank = (version: JsonRecord) =>
              version.tier === 'hires' || Number(version.bit_depth || 0) >= 24 ? 0 : 1;
            return tierRank(a) - tierRank(b);
          }),
        ...versions.filter((version) => version.provider === 'qobuz' && version.tier === 'catalog'),
        ...versions.filter(
          (version) => version.provider !== 'local' && version.provider !== 'qobuz'
        )
      ]
    : [];
  const fallbackVersion = fallbackAlbum
    ? [
        {
          id: fallbackAlbum.id || 'library',
          provider: 'local',
          source_label: 'Library',
          title: titleOf(fallbackAlbum, 'Album'),
          artist: fallbackAlbum.album_artist || fallbackAlbum.artist || 'Unknown artist',
          year: fallbackAlbum.year,
          track_count: fallbackTracks.length,
          sample_rate: Math.max(
            0,
            ...fallbackTracks.map((track) => Number(track.sample_rate) || 0)
          ),
          bit_depth: Math.max(16, ...fallbackTracks.map((track) => Number(track.bit_depth) || 0)),
          format: 'FLAC',
          art_id: fallbackAlbum.art_id,
          image_url: fallbackAlbum.image_url,
          is_primary: true
        } as JsonRecord
      ]
    : [];
  const rows = ordered.length ? ordered : fallbackVersion;

  useEffect(() => {
    if (!contextMenu) return undefined;
    const close = () => setContextMenu(null);
    window.addEventListener('click', close);
    window.addEventListener('keydown', close);
    window.addEventListener('resize', close);
    return () => {
      window.removeEventListener('click', close);
      window.removeEventListener('keydown', close);
      window.removeEventListener('resize', close);
    };
  }, [contextMenu]);

  return (
    <section className="versions-layout">
      <div className="version-section">
        <h2 className="version-section-title">
          {rows.length} version{rows.length === 1 ? '' : 's'}
        </h2>
        <div className="version-list">
          {rows.length ? (
            rows.map((version) => {
              const art = versionArt(version);
              const isPrimary = Boolean(version.is_primary);
              const isViewing =
                viewingVersionId === null || viewingVersionId === undefined
                  ? isPrimary
                  : sameVersionId(version.id, viewingVersionId);
              const providerLabel = String(
                version.source_label || (version.provider === 'qobuz' ? 'Qobuz' : 'Library')
              );
              const isQobuzVersion = version.provider === 'qobuz';
              const canEdit = version.provider === 'local' && Boolean(onEditLocalAlbum);
              const versionLabel = albumVersionLabel(version);
              const openAlbumId = idValue(version.open_album_id);
              const openLocalAlbumId = idValue(version.open_local_album_id);
              const canOpenAlbum = openAlbumId !== '' && Boolean(onOpenQobuzAlbum);
              const canOpenLocalAlbum = openLocalAlbumId !== '' && Boolean(onOpenLocalAlbum);
              const canViewVersion = version.id !== undefined && Boolean(onViewVersion);
              const openRow = () => {
                if (canOpenLocalAlbum) {
                  onOpenLocalAlbum?.(openLocalAlbumId);
                  return;
                }
                if (canOpenAlbum) {
                  onOpenQobuzAlbum?.(openAlbumId, version as LibraryAlbum);
                  return;
                }
                if (canViewVersion) onViewVersion?.(version.id as string | number);
              };
              const versionSubtitle = [
                String(
                  version.artist ||
                    fallbackAlbum?.album_artist ||
                    fallbackAlbum?.artist ||
                    'Unknown artist'
                ),
                version.year ? String(version.year) : '',
                versionLabel
              ]
                .filter(Boolean)
                .join(' · ');
              return (
                <article
                  className={`version-row${isPrimary ? ' is-primary' : ''}${isViewing ? ' is-viewing' : ''}`}
                  key={String(version.id || `${version.title}-${providerLabel}`)}
                  role={canOpenLocalAlbum || canOpenAlbum || canViewVersion ? 'button' : undefined}
                  tabIndex={canOpenLocalAlbum || canOpenAlbum || canViewVersion ? 0 : undefined}
                  onClick={openRow}
                  onContextMenu={(event) => {
                    if (!canEdit) return;
                    event.preventDefault();
                    event.stopPropagation();
                    setContextMenu(versionContextMenuPosition(event.clientX, event.clientY));
                  }}
                  onKeyDown={(event) => {
                    if (event.key !== 'Enter' && event.key !== ' ') return;
                    event.preventDefault();
                    openRow();
                  }}
                >
                  <div className="version-cover">
                    {art ? <img alt="" src={art} loading="lazy" /> : artFallback()}
                  </div>
                  <div className="version-main">
                    <div className="version-kicker">
                      {isViewing ? 'Currently viewing' : providerLabel}
                    </div>
                    <strong className="version-title">
                      <span>{String(version.title || titleOf(fallbackAlbum, 'Album'))}</span>
                      {isQobuzVersion ? <QobuzSourceIcon decorative /> : null}
                    </strong>
                    <span>{versionSubtitle}</span>
                  </div>
                  <div className="version-count">
                    {version.track_count ? `${version.track_count} tracks` : ''}
                  </div>
                  <div className="version-quality">{versionQualityLabel(version)}</div>
                  <div className="version-actions">
                    {isPrimary ? (
                      <span className="version-primary-badge">Primary</span>
                    ) : canOpenLocalAlbum ? (
                      <button
                        className="pill version-set-primary"
                        type="button"
                        onClick={(event) => {
                          event.stopPropagation();
                          onOpenLocalAlbum?.(openLocalAlbumId);
                        }}
                      >
                        Open
                      </button>
                    ) : canOpenAlbum ? (
                      <button
                        className="pill version-set-primary"
                        type="button"
                        onClick={(event) => {
                          event.stopPropagation();
                          onOpenQobuzAlbum?.(openAlbumId, version as LibraryAlbum);
                        }}
                      >
                        Open
                      </button>
                    ) : version.id !== undefined ? (
                      <button
                        className="pill version-set-primary version-set-primary-action"
                        type="button"
                        onClick={(event) => {
                          event.stopPropagation();
                          onSetPrimary(version.id as string | number);
                        }}
                      >
                        Set as primary
                      </button>
                    ) : null}
                  </div>
                </article>
              );
            })
          ) : (
            <div className="version-empty">No versions available.</div>
          )}
        </div>
      </div>
      {contextMenu ? (
        <Menu
          className="track-actions-menu track-actions-menu-wide version-context-menu is-open"
          ariaLabel="Local album options"
          style={{ left: contextMenu.x, top: contextMenu.y }}
          onClick={(event) => event.stopPropagation()}
        >
          <button
            className="track-action-item"
            type="button"
            role="menuitem"
            onClick={() => {
              setContextMenu(null);
              onEditLocalAlbum?.();
            }}
          >
            <Icon path="M12 20h9M16.5 3.5a2.1 2.1 0 0 1 3 3L7 19l-4 1 1-4Z" />
            <span>Edit metadata</span>
          </button>
        </Menu>
      ) : null}
    </section>
  );
}

import { useEffect, useMemo, useState } from 'react';
import { endpoints } from '../../../shared/lib/api';
import {
  albumArt,
  artistMatchesName,
  artistOf,
  dedupeDiscographyAlbums,
  descriptionParagraphs,
  discographyAlbumGroupKey,
  discographyBucket,
  discographyBuckets,
  idValue,
  normalizeQobuzAlbumId,
  normalizeSearchText,
  plainDescription,
  primaryArtistName,
  safeArray,
  titleOf
} from '../../../shared/lib/appSupport';
import { displayTitleUsesFallbackFont } from '../../../shared/lib/displayTitle';
import { stripFileExtension } from '../../../shared/lib/format';
import type { CustomDisplayFontSettings } from '../../../shared/lib/theme';
import type { JsonRecord, LibraryAlbum, LibraryTrack, QobuzTrack } from '../../../shared/types';
import { Icon } from '../../../shared/ui/Icon';
import { Menu } from '../../../shared/ui/Menu';
import { actionMenuPosition } from '../../../shared/ui/menuPosition';
import { PlaybarPlayIcon } from '../../../shared/ui/PlaybarPlayIcon';
import { useActionMenuScrollLock } from '../../../shared/ui/useActionMenuScrollLock';
import { AlbumDescriptionModal } from '../../albums/components/AlbumDescriptionModal';
import { AlbumGrid } from '../../albums/components/AlbumGrid';

export function localAlbumForDiscographyAlbum(album: LibraryAlbum, localAlbums: LibraryAlbum[]) {
  const qobuzIds = new Set(
    [
      normalizeQobuzAlbumId(album),
      ...safeArray<LibraryAlbum>(album.qobuz_album_versions).map(normalizeQobuzAlbumId)
    ]
      .filter(Boolean)
      .map(String)
  );
  if (qobuzIds.size) {
    const linked = localAlbums.find((localAlbum) =>
      qobuzIds.has(String(normalizeQobuzAlbumId(localAlbum)))
    );
    if (linked) return linked;
  }

  const groupKey = discographyAlbumGroupKey(album);
  return localAlbums.find((localAlbum) => discographyAlbumGroupKey(localAlbum) === groupKey);
}

export function ArtistDetailPage({
  name,
  albums,
  tracks,
  onOpenAlbum,
  onOpenQobuzAlbum,
  onOpenArtist,
  onPlayAlbum,
  onPlayArtistRadio,
  onPlayQobuzAlbum,
  onPlayTrack,
  onPlayQobuzTrack,
  customDisplayFont
}: {
  name: string;
  albums: LibraryAlbum[];
  tracks: LibraryTrack[];
  onOpenAlbum: (id: string | number) => void;
  onOpenQobuzAlbum: (id: string | number, album?: LibraryAlbum) => void;
  onOpenArtist: (name: string) => void;
  onPlayAlbum: (id: string | number) => Promise<void>;
  onPlayArtistRadio: (artistName: string) => Promise<void>;
  onPlayQobuzAlbum: (id: string | number) => Promise<void>;
  onPlayTrack: (track: LibraryTrack) => void;
  onPlayQobuzTrack: (track: QobuzTrack, related?: QobuzTrack[]) => void;
  customDisplayFont: CustomDisplayFontSettings | null;
}) {
  const [remoteArtist, setRemoteArtist] = useState<JsonRecord | null>(null);
  const [remoteAlbums, setRemoteAlbums] = useState<LibraryAlbum[]>([]);
  const [remoteTopTracks, setRemoteTopTracks] = useState<QobuzTrack[]>([]);
  const [similarArtists, setSimilarArtists] = useState<JsonRecord[]>([]);
  const [loadingArtist, setLoadingArtist] = useState(false);
  const [loadingTopTracks, setLoadingTopTracks] = useState(false);
  const [radioStarting, setRadioStarting] = useState(false);
  const [descriptionOpen, setDescriptionOpen] = useState(false);
  const [trackMenu, setTrackMenu] = useState<{ index: number; x: number; y: number } | null>(null);
  const [activeBucket, setActiveBucket] =
    useState<(typeof discographyBuckets)[number]['id']>('album');
  const lookupName = primaryArtistName(name) || name;
  useActionMenuScrollLock(Boolean(trackMenu));

  useEffect(() => {
    let cancelled = false;
    const query = primaryArtistName(name);
    setRemoteArtist(null);
    setRemoteAlbums([]);
    setRemoteTopTracks([]);
    setSimilarArtists([]);
    setDescriptionOpen(false);
    setTrackMenu(null);
    setLoadingTopTracks(false);
    if (!query)
      return () => {
        cancelled = true;
      };
    setLoadingArtist(true);

    const loadArtist = async () => {
      try {
        const search = await endpoints.qobuzArtistSearch(query, 3);
        let match =
          safeArray<JsonRecord>(search.artists)[0] ||
          safeArray<JsonRecord>((search.artists as JsonRecord | undefined)?.items)[0] ||
          safeArray<JsonRecord>(search.items)[0];

        if (!match) {
          const trackSearch = (await endpoints.qobuzSearch(query)) as unknown;
          const foundTracks = Array.isArray(trackSearch)
            ? (trackSearch as QobuzTrack[])
            : safeArray<QobuzTrack>((trackSearch as JsonRecord)?.tracks);
          const matchedTrack = foundTracks.find((track) => artistMatchesName(track.artist, query));
          if (matchedTrack?.artist) {
            const exact = await endpoints.qobuzArtistSearch(String(matchedTrack.artist), 1);
            match =
              safeArray<JsonRecord>(exact.artists)[0] ||
              safeArray<JsonRecord>((exact.artists as JsonRecord | undefined)?.items)[0] ||
              safeArray<JsonRecord>(exact.items)[0];
          }
        }

        const artistId = idValue(match?.id, match?.artist_id);
        if (!artistId) return;
        if (cancelled) return;

        if (match) setRemoteArtist(match);

        setLoadingTopTracks(true);
        endpoints
          .qobuzArtistTopTracks(artistId)
          .then((result) => {
            if (!cancelled)
              setRemoteTopTracks(
                safeArray<QobuzTrack>(result.top_tracks || result.tracks).slice(0, 9)
              );
          })
          .catch(() => {})
          .finally(() => {
            if (!cancelled) setLoadingTopTracks(false);
          });

        endpoints
          .qobuzArtistSimilar(artistId)
          .then((result) => {
            if (!cancelled) setSimilarArtists(safeArray<JsonRecord>(result.similar).slice(0, 20));
          })
          .catch(() => {});

        const coreResult = await endpoints
          .qobuzArtistCore(artistId)
          .catch(() => endpoints.qobuzArtist(artistId));
        if (cancelled) return;
        setRemoteArtist((coreResult.artist || coreResult) as JsonRecord);
        setRemoteAlbums(safeArray<LibraryAlbum>(coreResult.albums));
      } finally {
        if (!cancelled) setLoadingArtist(false);
      }
    };

    loadArtist().catch(() => {
      if (!cancelled) setLoadingArtist(false);
    });
    return () => {
      cancelled = true;
    };
  }, [name]);
  useEffect(() => {
    if (!descriptionOpen) return undefined;
    const onKey = (event: KeyboardEvent) => {
      if (event.key === 'Escape') setDescriptionOpen(false);
    };
    document.addEventListener('keydown', onKey);
    return () => document.removeEventListener('keydown', onKey);
  }, [descriptionOpen]);
  useEffect(() => {
    const closeMenu = () => setTrackMenu(null);
    window.addEventListener('click', closeMenu);
    window.addEventListener('keydown', closeMenu);
    return () => {
      window.removeEventListener('click', closeMenu);
      window.removeEventListener('keydown', closeMenu);
    };
  }, []);

  const displayName = String(remoteArtist?.name || lookupName || 'Unknown artist');
  const artistTracks = tracks.filter((track) => artistMatchesName(artistOf(track), lookupName));
  const localAlbums = useMemo(() => {
    const artistKey = normalizeSearchText(displayName);
    if (!artistKey) return [];
    const qobuzIds = new Set(
      remoteAlbums
        .map((album) => normalizeQobuzAlbumId(album))
        .filter((id) => String(id || '').trim())
        .map(String)
    );

    return albums
      .filter((album) => {
        const albumArtist = album.album_artist || album.artist;
        const localArtistKey = normalizeSearchText(albumArtist);
        const primaryLocalArtistKey = normalizeSearchText(primaryArtistName(albumArtist));
        if (localArtistKey === artistKey || primaryLocalArtistKey === artistKey) return true;

        const linkedId = album.qobuz_album_id ? normalizeQobuzAlbumId(album.qobuz_album_id) : '';
        return Boolean(linkedId && qobuzIds.has(String(linkedId)));
      })
      .sort(
        (a, b) => Number(b.year || 0) - Number(a.year || 0) || titleOf(a).localeCompare(titleOf(b))
      );
  }, [albums, displayName, remoteAlbums]);
  const grouped = useMemo(() => {
    const buckets = new Map<string, LibraryAlbum[]>(
      discographyBuckets.map((bucket) => [bucket.id, []])
    );
    remoteAlbums.forEach((album) => buckets.get(discographyBucket(album))?.push(album));
    discographyBuckets.forEach((bucket) => {
      if (bucket.id !== 'library')
        buckets.set(bucket.id, dedupeDiscographyAlbums(buckets.get(bucket.id) || []));
    });
    buckets.set('library', localAlbums);
    return buckets;
  }, [localAlbums, remoteAlbums]);
  const availableBuckets = discographyBuckets.filter(
    (bucket) => (grouped.get(bucket.id)?.length || 0) > 0
  );
  const displayBucket = availableBuckets.some((bucket) => bucket.id === activeBucket)
    ? activeBucket
    : availableBuckets[0]?.id || 'album';
  const displayAlbums = grouped.get(displayBucket) || [];
  const heroImage = typeof remoteArtist?.image_url === 'string' ? remoteArtist.image_url : '';
  const bioSource = remoteArtist?.biography;
  const bio = plainDescription(bioSource);
  const bioBlocks = descriptionParagraphs(bioSource);
  const stats = [remoteArtist?.genre ? String(remoteArtist.genre) : ''].filter(Boolean);
  const artistNameClass = artistHeroNameClass(displayName, customDisplayFont);
  const topItems = remoteTopTracks.length
    ? remoteTopTracks.map((track) => ({ kind: 'qobuz' as const, track }))
    : artistTracks.slice(0, 9).map((track) => ({ kind: 'local' as const, track }));
  const topTracksLoading = (loadingArtist || loadingTopTracks) && !remoteTopTracks.length;
  const showTopTracksSection = topTracksLoading || topItems.length > 0;
  const playTopItem = (item: (typeof topItems)[number]) => {
    if (item.kind === 'qobuz') onPlayQobuzTrack(item.track as QobuzTrack, remoteTopTracks);
    else onPlayTrack(item.track as LibraryTrack);
  };
  const openTopItemAlbum = (item: (typeof topItems)[number]) => {
    const albumId = topTrackAlbumId(item.track as JsonRecord);
    if (!albumId) return;
    if (item.kind === 'qobuz') onOpenQobuzAlbum(albumId);
    else onOpenAlbum(albumId);
  };
  const playRadio = () => {
    if (!displayName.trim() || radioStarting) return;
    setRadioStarting(true);
    onPlayArtistRadio(displayName).finally(() => setRadioStarting(false));
  };

  return (
    <section className="view artist-detail-view">
      <div className="artist-detail">
        <header className="artist-hero">
          <div className="artist-hero-image-frame">
            {heroImage ? (
              <img className="artist-hero-image" alt="" src={heroImage} loading="lazy" />
            ) : (
              <div className="artist-hero-image placeholder">
                {displayName.slice(0, 1).toUpperCase()}
              </div>
            )}
          </div>
          <div className="artist-hero-meta">
            <div className="artist-hero-primary">
              <h1 className={artistNameClass}>{displayName}</h1>
              {stats.length ? (
                <div className="artist-hero-stats">
                  {stats.map((stat, index) => (
                    <span className={index === 0 ? 'stamp ink no-dot' : 'ink-meta'} key={stat}>
                      {stat}
                    </span>
                  ))}
                </div>
              ) : null}
              <button
                className={`artist-radio-play-button${radioStarting ? ' is-loading' : ''}`}
                type="button"
                aria-label={`Play ${displayName} Radio`}
                aria-busy={radioStarting ? 'true' : undefined}
                disabled={!displayName.trim() || radioStarting}
                onClick={playRadio}
              >
                <Icon path="M8 5v14l11-7Z" />
                <span>{radioStarting ? 'Starting...' : 'Play Radio'}</span>
              </button>
            </div>
            {bio ? (
              <div className="artist-hero-bio">
                <button
                  className="album-detail-about artist-hero-bio-button"
                  type="button"
                  aria-label="Read full artist biography"
                  onClick={() => setDescriptionOpen(true)}
                >
                  <div className="album-detail-about-text artist-hero-bio-text">{bio}</div>
                  <span className="album-detail-about-more" aria-hidden="true">
                    More
                  </span>
                </button>
              </div>
            ) : null}
          </div>
        </header>

        {showTopTracksSection ? (
          <section className="artist-section artist-top-tracks">
            <header className="artist-section-head">
              <div>
                <h2 className="artist-section-title">Top tracks</h2>
              </div>
            </header>
            {topTracksLoading ? (
              <ArtistTopTracksSkeleton />
            ) : (
              <ul className="file-list song-list artist-top-tracks-list">
                {topItems.map((item, index) => {
                  const track = item.track;
                  const cover = albumArt(track);
                  const title = titleOf(track, stripFileExtension(String(track.file_name || '')));
                  const album = topTrackAlbumTitle(track as JsonRecord);
                  return (
                    <li
                      className="file-item album-track-item songs-track-row artist-top-track-row"
                      key={String(
                        track.id || track.track_id || track.file_name || `${title}-${index}`
                      )}
                      onClick={() => playTopItem(item)}
                    >
                      <div className="album-track-index songs-track-art-cell">
                        <span
                          className={`songs-track-art${cover ? ' has-cover' : ''}`}
                          aria-hidden="true"
                        >
                          {cover ? (
                            <img alt="" src={cover} loading="lazy" />
                          ) : (
                            <Icon path="M9 18V5l12-2v13M9 18a3 3 0 1 1-6 0 3 3 0 0 1 6 0Zm12-2a3 3 0 1 1-6 0 3 3 0 0 1 6 0Z" />
                          )}
                        </span>
                        <button
                          className="btn-item-play"
                          type="button"
                          aria-label={`Play ${title}`}
                          onClick={(event) => {
                            event.stopPropagation();
                            playTopItem(item);
                          }}
                        >
                          <svg
                            className="artist-top-track-play-icon"
                            viewBox="0 0 100 100"
                            aria-hidden="true"
                            shapeRendering="geometricPrecision"
                          >
                            <path d="M 39 32 C 36.2 33.3 35 35.7 35 39 L 35 61 C 35 64.3 36.2 66.7 39 68 C 41.4 69.1 43.5 68.2 46.2 66.6 L 66.4 54.4 C 71.2 51.5 71.2 48.5 66.4 45.6 L 46.2 33.4 C 43.5 31.8 41.4 30.9 39 32 Z" />
                          </svg>
                        </button>
                      </div>
                      <div className="file-details songs-track-details">
                        <span className="file-name" title={title}>
                          {title}
                        </span>
                        <span className="file-subline artist-top-track-album" title={album}>
                          <span>{album}</span>
                        </span>
                      </div>
                      <button
                        className="btn-item-more"
                        type="button"
                        title="More options"
                        aria-label={`More options for ${title}`}
                        onClick={(event) => {
                          event.stopPropagation();
                          setTrackMenu({
                            index,
                            ...actionMenuPosition(event.currentTarget.getBoundingClientRect(), {
                              menuHeight: 84
                            })
                          });
                        }}
                      >
                        <svg
                          viewBox="0 0 24 24"
                          width="16"
                          height="16"
                          fill="none"
                          stroke="currentColor"
                          strokeWidth="2"
                          strokeLinecap="round"
                          strokeLinejoin="round"
                        >
                          <circle cx="12" cy="12" r="1" />
                          <circle cx="12" cy="5" r="1" />
                          <circle cx="12" cy="19" r="1" />
                        </svg>
                      </button>
                    </li>
                  );
                })}
              </ul>
            )}
          </section>
        ) : null}

        {availableBuckets.length ? (
          <section className="artist-section">
            <header className="artist-section-head">
              <div>
                <h2 className="artist-section-title">Discography</h2>
              </div>
            </header>
            <nav className="segmented discography-filter" aria-label="Discography filter">
              {availableBuckets.map((bucket) => (
                <button
                  className={displayBucket === bucket.id ? 'on' : ''}
                  type="button"
                  key={bucket.id}
                  onClick={() => setActiveBucket(bucket.id)}
                >
                  {bucket.label}
                </button>
              ))}
            </nav>
            <AlbumGrid
              albums={displayAlbums}
              showArtist={displayBucket === 'library'}
              onOpenArtist={onOpenArtist}
              onPlay={(album) => {
                const localAlbum =
                  displayBucket === 'library'
                    ? album
                    : localAlbumForDiscographyAlbum(album, localAlbums);
                if (localAlbum) {
                  const localId = idValue(localAlbum.id, localAlbum.album_id);
                  if (localId !== '') onPlayAlbum(localId);
                  return;
                }
                const playId = idValue(album.id, album.qobuz_album_id, album.album_id);
                onPlayQobuzAlbum(playId);
              }}
              onOpen={(album) => {
                const localAlbum =
                  displayBucket === 'library'
                    ? album
                    : localAlbumForDiscographyAlbum(album, localAlbums);
                if (localAlbum) {
                  const localId = idValue(localAlbum.id, localAlbum.album_id);
                  if (localId !== '') onOpenAlbum(localId);
                  return;
                }
                const openId = idValue(album.id, album.qobuz_album_id, album.album_id);
                onOpenQobuzAlbum(openId, album);
              }}
            />
          </section>
        ) : null}

        {similarArtists.length ? (
          <section className="artist-section">
            <header className="artist-section-head">
              <div>
                <h2 className="artist-section-title">Fans also listened to</h2>
              </div>
            </header>
            <div className="artist-similar-row">
              {similarArtists.map((artist, index) => {
                const artistName = String(artist.name || artist.title || 'Unknown artist');
                const image = typeof artist.image_url === 'string' ? artist.image_url : '';
                return (
                  <button
                    className="artist-similar-card"
                    type="button"
                    key={String(artist.id || artistName || index)}
                    onClick={() => onOpenArtist(artistName)}
                  >
                    {image ? (
                      <img className="artist-similar-image" alt="" src={image} loading="lazy" />
                    ) : (
                      <div className="artist-similar-image" />
                    )}
                    <div className="artist-similar-name" title={artistName}>
                      {artistName}
                    </div>
                  </button>
                );
              })}
            </div>
          </section>
        ) : null}

        {!loadingArtist && !topItems.length && !availableBuckets.length ? (
          <div className="file-limits">No artist data.</div>
        ) : null}
        {descriptionOpen ? (
          <AlbumDescriptionModal
            title={displayName}
            label="About this artist"
            paragraphs={bioBlocks.length ? bioBlocks : [bio]}
            onClose={() => setDescriptionOpen(false)}
          />
        ) : null}
        {trackMenu && topItems[trackMenu.index] ? (
          <Menu
            className="track-actions-menu track-actions-menu-wide is-open"
            ariaLabel="Track options"
            style={{ left: Math.max(12, trackMenu.x), top: trackMenu.y }}
            onClick={(event) => event.stopPropagation()}
          >
            <button
              className="track-action-item has-filled-icon"
              type="button"
              role="menuitem"
              onClick={() => {
                playTopItem(topItems[trackMenu.index]);
                setTrackMenu(null);
              }}
            >
              <PlaybarPlayIcon className="track-action-play-icon" />
              <span>Play</span>
            </button>
            {topTrackAlbumId(topItems[trackMenu.index].track as JsonRecord) ? (
              <button
                className="track-action-item"
                type="button"
                role="menuitem"
                onClick={() => {
                  openTopItemAlbum(topItems[trackMenu.index]);
                  setTrackMenu(null);
                }}
              >
                <Icon path="M5 4h14v16H5zM12 9a3 3 0 1 0 0 6 3 3 0 0 0 0-6ZM12 12h.01" />
                <span>Go to album</span>
              </button>
            ) : null}
          </Menu>
        ) : null}
      </div>
    </section>
  );
}

function artistHeroNameClass(name: string, customDisplayFont: CustomDisplayFontSettings | null) {
  const lengthClass =
    name.length > 58 ? ' is-extra-long-title' : name.length > 38 ? ' is-long-title' : '';
  const fallbackClass = displayTitleUsesFallbackFont(name, customDisplayFont)
    ? ' uses-fallback-font'
    : '';
  return `artist-hero-name${lengthClass}${fallbackClass}`;
}

function ArtistTopTracksSkeleton() {
  return (
    <ul
      className="file-list song-list artist-top-tracks-list artist-top-tracks-skeleton"
      aria-busy="true"
      aria-label="Loading top tracks"
    >
      {Array.from({ length: 9 }, (_, index) => (
        <li
          className="file-item album-track-item songs-track-row artist-top-track-row artist-top-track-skeleton-row"
          key={index}
        >
          <span className="songs-track-art skeleton-shimmer" />
          <span className="artist-top-track-skeleton-copy" aria-hidden="true">
            <span className="artist-top-track-skeleton-title skeleton-shimmer" />
            <span className="artist-top-track-skeleton-album skeleton-shimmer" />
          </span>
          <span className="btn-item-more artist-top-track-skeleton-more skeleton-shimmer" />
        </li>
      ))}
    </ul>
  );
}

function topTrackAlbumId(track: JsonRecord) {
  const album = track.album;
  const nestedId = album && typeof album === 'object' ? (album as JsonRecord).id : null;
  return idValue(track.album_id, track.qobuz_album_id, nestedId);
}

function topTrackAlbumTitle(track: JsonRecord) {
  const album = track.album;
  if (typeof album === 'string' && album.trim()) return album;
  if (album && typeof album === 'object') return titleOf(album as JsonRecord, 'Top track');
  return 'Top track';
}

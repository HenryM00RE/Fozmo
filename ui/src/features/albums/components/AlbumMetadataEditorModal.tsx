import { useEffect, useMemo, useRef, useState } from 'react';
import { api, endpoints } from '../../../shared/lib/api';
import {
  albumArt,
  artFallback,
  qobuzAlbumQualityLabel,
  safeArray,
  titleOf
} from '../../../shared/lib/appSupport';
import type { JsonRecord, LibraryAlbum, LibraryTrack } from '../../../shared/types';
import { Icon } from '../../../shared/ui/Icon';
import { Modal } from '../../../shared/ui/Modal';
import {
  errorMessage,
  initialQobuzQuery,
  normalizeQobuzAlbum,
  type QobuzAlbumChoice,
  qobuzIdKey
} from '../../settings/model/metadataAssignerModel';
import {
  albumEditorHasChanges,
  albumEditorInitialArtist,
  albumEditorInitialTitle,
  albumEditorInitialYear,
  albumEditorMetadataHasChanges,
  albumEditorYearPayload,
  type EditableAlbumTrack,
  editableAlbumTracks,
  moveAlbumTrack,
  trackEditPayload,
  updateAlbumTrackTitle
} from '../model/albumMetadataEditorModel';

type AlbumMetadataEditorModalProps = {
  album: LibraryAlbum;
  tracks: LibraryTrack[];
  onClose: () => void;
  onSaved: (detail: JsonRecord) => void;
};

export function AlbumMetadataEditorModal({
  album,
  tracks,
  onClose,
  onSaved
}: AlbumMetadataEditorModalProps) {
  const initialTracks = useMemo(() => editableAlbumTracks(tracks), [tracks]);
  const [title, setTitle] = useState(() => albumEditorInitialTitle(album));
  const [albumArtist, setAlbumArtist] = useState(() => albumEditorInitialArtist(album));
  const [year, setYear] = useState(() => albumEditorInitialYear(album));
  const [orderedTracks, setOrderedTracks] = useState<EditableAlbumTrack[]>(initialTracks);
  const [customCoverFile, setCustomCoverFile] = useState<File | null>(null);
  const [customCoverPreview, setCustomCoverPreview] = useState('');
  const [draggingKey, setDraggingKey] = useState<string | null>(null);
  const [query, setQuery] = useState(() => initialQobuzQuery(album));
  const [results, setResults] = useState<QobuzAlbumChoice[]>([]);
  const [selectedQobuz, setSelectedQobuz] = useState<QobuzAlbumChoice | null>(null);
  const [searchMessage, setSearchMessage] = useState('Search Qobuz to link this local album.');
  const [searching, setSearching] = useState(false);
  const [saving, setSaving] = useState(false);
  const [unlinking, setUnlinking] = useState(false);
  const [message, setMessage] = useState('');
  const [linkedQobuz, setLinkedQobuz] = useState<QobuzAlbumChoice | null>(null);
  const [linkedQobuzError, setLinkedQobuzError] = useState('');
  const requestTokenRef = useRef(0);
  const coverInputRef = useRef<HTMLInputElement | null>(null);

  const linkedQobuzId = String(album.qobuz_album_id || album.qobuz_id || '').trim();
  const selectedQobuzId = selectedQobuz?.id || null;
  const replacementQobuzId =
    selectedQobuzId && qobuzIdKey(selectedQobuzId) !== qobuzIdKey(linkedQobuzId)
      ? selectedQobuzId
      : null;
  const hasChanges = albumEditorHasChanges(
    album,
    initialTracks,
    orderedTracks,
    title,
    albumArtist,
    year,
    replacementQobuzId,
    Boolean(customCoverFile)
  );
  const hasMetadataChanges = albumEditorMetadataHasChanges(
    album,
    initialTracks,
    orderedTracks,
    title,
    albumArtist,
    year
  );
  const coverSrc = customCoverPreview || albumArt(album);

  useEffect(() => {
    setTitle(albumEditorInitialTitle(album));
    setAlbumArtist(albumEditorInitialArtist(album));
    setYear(albumEditorInitialYear(album));
    setOrderedTracks(initialTracks);
    setCustomCoverFile(null);
    setQuery(initialQobuzQuery(album));
    setResults([]);
    setSelectedQobuz(null);
    setSearchMessage('Search Qobuz to link this local album.');
    setMessage('');
    setLinkedQobuz(null);
    setLinkedQobuzError('');
  }, [album, initialTracks]);

  useEffect(() => {
    if (!customCoverFile) {
      setCustomCoverPreview('');
      return undefined;
    }
    const preview = URL.createObjectURL(customCoverFile);
    setCustomCoverPreview(preview);
    return () => URL.revokeObjectURL(preview);
  }, [customCoverFile]);

  useEffect(() => {
    if (!linkedQobuzId) {
      setLinkedQobuz(null);
      setLinkedQobuzError('');
      return undefined;
    }
    let active = true;
    setLinkedQobuz(null);
    setLinkedQobuzError('');
    endpoints
      .qobuzAlbum(linkedQobuzId)
      .then((response) => {
        if (!active) return;
        const normalized = normalizeQobuzAlbum((response as JsonRecord).album || response);
        if (normalized) {
          setLinkedQobuz(normalized);
        } else {
          setLinkedQobuzError(`Qobuz album ${linkedQobuzId} is linked.`);
        }
      })
      .catch(() => {
        if (active) setLinkedQobuzError(`Qobuz album ${linkedQobuzId} is linked.`);
      });
    return () => {
      active = false;
    };
  }, [linkedQobuzId]);

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape') onClose();
    };
    document.addEventListener('keydown', onKeyDown);
    return () => document.removeEventListener('keydown', onKeyDown);
  }, [onClose]);

  const searchQobuz = async (rawQuery = query) => {
    const nextQuery = rawQuery.trim();
    if (!nextQuery) {
      setResults([]);
      setSearchMessage('Enter an album or artist to search.');
      return;
    }
    const token = ++requestTokenRef.current;
    setSearching(true);
    setSearchMessage('Searching Qobuz...');
    try {
      const response = await api.get<JsonRecord>('/api/qobuz/search/albums', { q: nextQuery });
      if (token !== requestTokenRef.current) return;
      const albums = safeArray<unknown>(response.albums || response)
        .map(normalizeQobuzAlbum)
        .filter(Boolean) as QobuzAlbumChoice[];
      setResults(albums);
      setSearchMessage(albums.length ? '' : 'No Qobuz albums found.');
    } catch (error) {
      if (token !== requestTokenRef.current) return;
      setResults([]);
      setSearchMessage(`Qobuz search failed. ${errorMessage(error)}`);
    } finally {
      if (token === requestTokenRef.current) setSearching(false);
    }
  };

  const moveTrack = (fromIndex: number, toIndex: number) => {
    setOrderedTracks((current) => moveAlbumTrack(current, fromIndex, toIndex));
  };

  const changeTrackTitle = (editorKey: string, nextTitle: string) => {
    setOrderedTracks((current) => updateAlbumTrackTitle(current, editorKey, nextTitle));
  };

  const chooseCover = (file: File | null | undefined) => {
    if (!file) return;
    const allowedCoverTypes = new Set(['image/jpeg', 'image/png', 'image/webp']);
    if (!allowedCoverTypes.has(file.type)) {
      setMessage('Album art must be a JPEG, PNG, or WebP image.');
      if (coverInputRef.current) coverInputRef.current.value = '';
      return;
    }
    setMessage('');
    setCustomCoverFile(file);
  };

  const clearCoverSelection = () => {
    setCustomCoverFile(null);
    if (coverInputRef.current) coverInputRef.current.value = '';
  };

  const unlinkQobuz = async () => {
    if (!album.id || !linkedQobuzId || unlinking) return;
    setUnlinking(true);
    setMessage('');
    try {
      const detail = await endpoints.albumQobuzUnlink(album.id);
      setSelectedQobuz(null);
      onSaved(detail);
    } catch (error) {
      setMessage(`Qobuz link could not be removed. ${errorMessage(error)}`);
    } finally {
      setUnlinking(false);
    }
  };

  const save = async () => {
    const nextTitle = title.trim();
    const nextArtist = albumArtist.trim();
    const nextYearText = year.trim();
    const nextYear = albumEditorYearPayload(nextYearText);
    if (!album.id || saving) return;
    if (!nextTitle) {
      setMessage('Album title is required.');
      return;
    }
    if (
      nextYearText &&
      (nextYear === null ||
        !Number.isInteger(nextYear) ||
        nextYear < 1000 ||
        nextYear > new Date().getFullYear() + 1)
    ) {
      setMessage('Release year must be a four-digit year.');
      return;
    }
    if (orderedTracks.some((track) => !titleOf(track, '').trim())) {
      setMessage('Song titles are required.');
      return;
    }
    setSaving(true);
    setMessage('');
    try {
      let detail: JsonRecord | null = null;
      if (hasMetadataChanges) {
        detail = await api.put<JsonRecord>(
          `/api/library/albums/${encodeURIComponent(String(album.id))}`,
          {
            title: nextTitle,
            album_artist: nextArtist || null,
            year: nextYear,
            tracks: trackEditPayload(orderedTracks)
          }
        );
      }
      if (replacementQobuzId) {
        detail = await endpoints.albumQobuzLink(album.id, replacementQobuzId);
      }
      if (customCoverFile) {
        detail = await endpoints.uploadAlbumCover(album.id, customCoverFile);
      }
      if (detail) onSaved(detail);
      onClose();
    } catch (error) {
      setMessage(`Metadata could not be saved. ${errorMessage(error)}`);
    } finally {
      setSaving(false);
    }
  };

  return (
    <Modal
      open
      className="album-metadata-editor-backdrop"
      ariaLabelledBy="album-metadata-editor-title"
      onClose={onClose}
    >
      <section
        className="album-metadata-editor-panel app-modal-surface"
        onMouseDown={(event) => event.stopPropagation()}
      >
        <header className="metadata-assigner-head">
          <div>
            <strong id="album-metadata-editor-title">Edit album metadata</strong>
            <span>{titleOf(album, 'Local album')}</span>
          </div>
          <button
            className="metadata-assigner-close"
            type="button"
            aria-label="Close"
            onClick={onClose}
          >
            <Icon path="M18 6 6 18M6 6l12 12" />
          </button>
        </header>

        <div className="album-metadata-editor-body">
          {message ? <div className="metadata-assigner-message">{message}</div> : null}
          <div className="album-metadata-editor-content">
            <div className="album-metadata-editor-left">
              <section className="album-metadata-editor-summary">
                <div className="metadata-assigner-cover">
                  {coverSrc ? <img alt="" src={coverSrc} /> : artFallback()}
                </div>
                <div className="metadata-assigner-album-text">
                  <span className="section-label">Local album</span>
                  <strong>{titleOf(album, 'Album')}</strong>
                  <span className="album-subtitle">
                    {[album.album_artist || album.artist || 'Unknown artist', album.year]
                      .filter(Boolean)
                      .join(' / ')}
                  </span>
                  <div className="album-metadata-editor-cover-actions">
                    <button
                      className="pill"
                      type="button"
                      onClick={() => coverInputRef.current?.click()}
                    >
                      Choose art
                    </button>
                    {customCoverFile ? (
                      <button className="pill" type="button" onClick={clearCoverSelection}>
                        Clear
                      </button>
                    ) : null}
                    <span className="album-subtitle">
                      {customCoverFile
                        ? customCoverFile.name
                        : albumArt(album)
                          ? 'Current artwork'
                          : 'No artwork'}
                    </span>
                    <input
                      ref={coverInputRef}
                      type="file"
                      accept="image/jpeg,image/png,image/webp"
                      onChange={(event) => chooseCover(event.target.files?.[0])}
                    />
                  </div>
                </div>
              </section>

              <section className="metadata-assigner-qobuz">
                <LinkedQobuzStatus
                  linked={linkedQobuz}
                  linkedId={linkedQobuzId}
                  loadError={linkedQobuzError}
                  replacement={selectedQobuz}
                  unlinking={unlinking}
                  onUnlink={unlinkQobuz}
                />
                <div className="metadata-assigner-search-row">
                  <input
                    type="search"
                    value={query}
                    onChange={(event) => setQuery(event.target.value)}
                    onKeyDown={(event) => {
                      if (event.key === 'Enter') {
                        event.preventDefault();
                        searchQobuz();
                      }
                    }}
                    placeholder="Search Qobuz albums"
                  />
                  <button
                    className="pill primary"
                    type="button"
                    onClick={() => searchQobuz()}
                    disabled={searching}
                  >
                    Search
                  </button>
                </div>
                <SelectedQobuz
                  selected={selectedQobuz}
                  emptyLabel={
                    linkedQobuzId
                      ? 'No replacement Qobuz album selected.'
                      : 'No Qobuz album selected.'
                  }
                  onClear={() => setSelectedQobuz(null)}
                />
                <div className="match-candidates-header">
                  <strong>Qobuz albums</strong>
                  <span className="album-subtitle">Select an album match</span>
                </div>
                <div className="metadata-assigner-results">
                  {results.length ? (
                    results.map((result) => (
                      <QobuzResult
                        key={result.id}
                        album={result}
                        selected={Boolean(
                          selectedQobuz && qobuzIdKey(selectedQobuz.id) === qobuzIdKey(result.id)
                        )}
                        onSelect={() => {
                          setSelectedQobuz(result);
                          if (result.title) setTitle(result.title);
                          if (result.artist) setAlbumArtist(result.artist);
                          if (result.year) setYear(String(result.year));
                        }}
                      />
                    ))
                  ) : (
                    <div className="match-empty">
                      <span className="album-subtitle">{searchMessage}</span>
                    </div>
                  )}
                </div>
              </section>
            </div>

            <section className="album-metadata-editor-local">
              <div className="metadata-assigner-fields">
                <label className="field">
                  <span>Album title</span>
                  <input
                    type="text"
                    value={title}
                    onChange={(event) => setTitle(event.target.value)}
                    autoComplete="off"
                  />
                </label>
                <div className="album-metadata-editor-field-row">
                  <label className="field">
                    <span>Album artist</span>
                    <input
                      type="text"
                      value={albumArtist}
                      onChange={(event) => setAlbumArtist(event.target.value)}
                      autoComplete="off"
                    />
                  </label>
                  <label className="field">
                    <span>Release year</span>
                    <input
                      type="text"
                      value={year}
                      inputMode="numeric"
                      maxLength={4}
                      onChange={(event) =>
                        setYear(event.target.value.replace(/\D/g, '').slice(0, 4))
                      }
                      autoComplete="off"
                    />
                  </label>
                </div>
              </div>

              <div className="album-metadata-editor-tracks">
                <div className="match-candidates-header">
                  <strong>Song positions</strong>
                  <span className="album-subtitle">
                    {orderedTracks.length} {orderedTracks.length === 1 ? 'song' : 'songs'}
                  </span>
                </div>
                <ol>
                  {orderedTracks.map((track, index) => (
                    <li
                      className={`album-metadata-editor-track${draggingKey === track.editorKey ? ' is-dragging' : ''}`}
                      draggable
                      key={track.editorKey}
                      onDragStart={(event) => {
                        if (event.target instanceof HTMLInputElement) {
                          event.preventDefault();
                          return;
                        }
                        setDraggingKey(track.editorKey);
                        event.dataTransfer.effectAllowed = 'move';
                        event.dataTransfer.setData('text/plain', String(index));
                      }}
                      onDragOver={(event) => {
                        event.preventDefault();
                        event.dataTransfer.dropEffect = 'move';
                      }}
                      onDrop={(event) => {
                        event.preventDefault();
                        const fromIndex = Number(event.dataTransfer.getData('text/plain'));
                        if (Number.isFinite(fromIndex)) moveTrack(fromIndex, index);
                        setDraggingKey(null);
                      }}
                      onDragEnd={() => setDraggingKey(null)}
                    >
                      <span className="album-metadata-editor-track-handle" aria-hidden="true">
                        <Icon path="M8 6h.01M8 12h.01M8 18h.01M16 6h.01M16 12h.01M16 18h.01" />
                      </span>
                      <span className="album-metadata-editor-track-number">{index + 1}</span>
                      <span className="album-metadata-editor-track-main">
                        <input
                          className="album-metadata-editor-track-title-input"
                          type="text"
                          value={titleOf(track, '')}
                          aria-label={`Song ${index + 1} title`}
                          onChange={(event) =>
                            changeTrackTitle(track.editorKey, event.target.value)
                          }
                          onMouseDown={(event) => event.stopPropagation()}
                          autoComplete="off"
                        />
                        <small>
                          {track.artist ||
                            albumArtist ||
                            album.album_artist ||
                            album.artist ||
                            'Unknown artist'}
                        </small>
                      </span>
                    </li>
                  ))}
                </ol>
              </div>
            </section>
          </div>
        </div>

        <footer className="metadata-assigner-footer album-metadata-editor-footer">
          <button className="pill" type="button" onClick={onClose}>
            Cancel
          </button>
          <button
            className="pill primary"
            type="button"
            onClick={save}
            disabled={saving || !hasChanges}
          >
            {saving ? 'Saving...' : 'Save changes'}
          </button>
        </footer>
      </section>
    </Modal>
  );
}

function LinkedQobuzStatus({
  linked,
  linkedId,
  loadError,
  replacement,
  unlinking,
  onUnlink
}: {
  linked: QobuzAlbumChoice | null;
  linkedId: string;
  loadError: string;
  replacement: QobuzAlbumChoice | null;
  unlinking: boolean;
  onUnlink: () => void;
}) {
  if (!linkedId) {
    return (
      <div className="album-metadata-editor-link-status">
        <span className="section-label">Qobuz link</span>
        <strong>No Qobuz album linked.</strong>
      </div>
    );
  }

  return (
    <div className="album-metadata-editor-link-status">
      <div>
        <span className="section-label">Qobuz link</span>
        <strong>Already linked</strong>
        {replacement ? (
          <span className="album-subtitle">
            The selected Qobuz album will replace this link when you save.
          </span>
        ) : null}
      </div>
      <div className="album-metadata-editor-linked-card">
        {linked ? (
          <QobuzCover album={linked} />
        ) : (
          <span className="qobuz-link-cover">{artFallback()}</span>
        )}
        <span className="match-row-title">
          {linked?.title || loadError || `Qobuz album ${linkedId}`}
          <span className="album-subtitle">
            {linked
              ? [linked.artist, linked.year, qobuzAlbumQualityLabel(linked)]
                  .filter(Boolean)
                  .join(' / ')
              : linkedId}
          </span>
        </span>
        <button className="pill" type="button" onClick={onUnlink} disabled={unlinking}>
          {unlinking ? 'Unlinking...' : 'Unlink'}
        </button>
      </div>
    </div>
  );
}

function SelectedQobuz({
  selected,
  emptyLabel,
  onClear
}: {
  selected: QobuzAlbumChoice | null;
  emptyLabel: string;
  onClear: () => void;
}) {
  if (!selected)
    return (
      <div className="metadata-assigner-selected">
        <span className="album-subtitle">{emptyLabel}</span>
      </div>
    );
  return (
    <div className="metadata-assigner-selected">
      <div className="metadata-assigner-selected-card">
        <QobuzCover album={selected} />
        <span className="match-row-title">
          {selected.title || 'Untitled'}
          <span className="album-subtitle">
            {[selected.artist, selected.year, qobuzAlbumQualityLabel(selected)]
              .filter(Boolean)
              .join(' / ')}
          </span>
        </span>
        <button className="pill" type="button" onClick={onClear}>
          Clear
        </button>
      </div>
    </div>
  );
}

function QobuzResult({
  album,
  selected,
  onSelect
}: {
  album: QobuzAlbumChoice;
  selected: boolean;
  onSelect: () => void;
}) {
  return (
    <div className={`metadata-assigner-result${selected ? ' is-inspected' : ''}`}>
      <QobuzCover album={album} />
      <span className="match-row-title">
        {album.title || 'Untitled'}
        <span className="album-subtitle">
          {[album.artist, album.year].filter(Boolean).join(' / ')}
        </span>
      </span>
      <span className="match-row-score">{qobuzAlbumQualityLabel(album)}</span>
      <button className={`pill${selected ? '' : ' primary'}`} type="button" onClick={onSelect}>
        {selected ? 'Selected' : 'Select'}
      </button>
    </div>
  );
}

function QobuzCover({ album }: { album: QobuzAlbumChoice }) {
  return album.image_url ? (
    <img className="qobuz-link-cover" alt="" src={album.image_url} loading="lazy" />
  ) : (
    <span className="qobuz-link-cover">{artFallback()}</span>
  );
}

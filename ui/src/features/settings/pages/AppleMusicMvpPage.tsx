import { useCallback, useEffect, useState } from 'react';
import { endpoints } from '../../../shared/lib/api';
import { sourceRefToQueueItem } from '../../../shared/lib/queue';
import type { JsonRecord, QueueItem, SourceRef } from '../../../shared/types';

export function AppleMusicMvpPage({
  activeZoneStatus,
  addItemsToQueue
}: {
  activeZoneStatus: JsonRecord;
  addItemsToQueue: (items: QueueItem[], placement: 'next' | 'end') => Promise<boolean>;
}) {
  const [status, setStatus] = useState<JsonRecord | null>(null);
  const [busy, setBusy] = useState('');
  const [message, setMessage] = useState('');
  const [storefront, setStorefront] = useState('nz');
  const [searchTerm, setSearchTerm] = useState('');
  const [songs, setSongs] = useState<JsonRecord[]>([]);

  const activeZoneID = String(activeZoneStatus.active_zone_id || '');
  const activeZoneName = String(activeZoneStatus.active_zone_name || 'the active output');
  const helperRunning = numberValue(status?.helper_pid) !== null;
  const authorized = status?.authorization === 'authorized';
  const ready = status?.state === 'ready' && authorized;

  const refresh = useCallback(async () => {
    const next = await endpoints.appleMusicStatus();
    setStatus(next);
    return next;
  }, []);

  useEffect(() => {
    refresh().catch((error) => setMessage(errorMessage(error)));
  }, [refresh]);

  const run = async (key: string, action: () => Promise<unknown>, success: string) => {
    if (busy) return;
    setBusy(key);
    setMessage('');
    try {
      await action();
      await refresh();
      setMessage(success);
    } catch (error) {
      setMessage(errorMessage(error));
      await refresh().catch(() => undefined);
    } finally {
      setBusy('');
    }
  };

  const search = () =>
    run(
      'search',
      async () => {
        const result = await endpoints.appleMusicCatalogSearch(
          searchTerm.trim(),
          storefront.trim(),
          12
        );
        setSongs(recordArray(result.songs));
      },
      'Apple Music search complete.'
    );

  const play = (song: JsonRecord) =>
    run(
      `play-${String(song.song_id || '')}`,
      () => endpoints.playAppleMusicScenario(activeZoneID, catalogSongSource(song), []),
      `${String(song.title || 'Apple Music track')} started on ${activeZoneName}.`
    );

  const queue = (song: JsonRecord, placement: 'next' | 'end') =>
    run(
      `queue-${placement}-${String(song.song_id || '')}`,
      async () => {
        const item = sourceRefToQueueItem(catalogSongSource(song));
        if (!item) throw new Error('Could not convert this Apple Music track into a queue item.');
        if (!(await addItemsToQueue([item], placement))) {
          throw new Error('Could not update the playback queue.');
        }
      },
      `${String(song.title || 'Apple Music track')} added ${
        placement === 'next' ? 'to play next' : 'to the end of the queue'
      }.`
    );

  return (
    <section className="settings-panel apple-music-page">
      <div className="settings-section-heading">
        <div>
          <span className="section-label">Apple Music</span>
          <h2>Music.app playback</h2>
          <p>
            MusicKit supplies the catalog and authorization. Music.app decodes lossless audio into
            Fozmo Capture, then Fozmo sends it through the selected local output and DSP.
          </p>
        </div>
        <button
          className="pill"
          disabled={Boolean(busy)}
          onClick={() => refresh().catch((error) => setMessage(errorMessage(error)))}
          type="button"
        >
          Refresh
        </button>
      </div>

      <div className="settings-summary-grid">
        <StatusItem label="Helper" value={helperRunning ? 'Running' : 'Stopped'} />
        <StatusItem label="Authorization" value={String(status?.authorization || 'Checking')} />
        <StatusItem label="Catalog" value={ready ? 'Ready' : 'Unavailable'} />
        <StatusItem label="Output" value={activeZoneName} />
      </div>

      {status?.helper_present === false ? (
        <p className="settings-notice error">The Apple Music helper is missing from this build.</p>
      ) : null}
      {status?.last_error ? (
        <p className="settings-notice error">{errorMessage(status.last_error)}</p>
      ) : null}
      {message ? <p className="settings-notice">{message}</p> : null}

      <div className="settings-actions">
        <button
          className="pill primary"
          disabled={Boolean(busy) || helperRunning}
          onClick={() =>
            run('launch', endpoints.launchAppleMusicHelper, 'Apple Music helper launched.')
          }
          type="button"
        >
          Launch helper
        </button>
        <button
          className="pill"
          disabled={Boolean(busy) || !helperRunning || authorized}
          onClick={() =>
            run('authorize', endpoints.authorizeAppleMusic, 'Apple Music authorization updated.')
          }
          type="button"
        >
          Authorize Apple Music
        </button>
        <button
          className="pill"
          disabled={Boolean(busy) || !helperRunning}
          onClick={() =>
            run('shutdown', endpoints.shutdownAppleMusicHelper, 'Apple Music helper stopped.')
          }
          type="button"
        >
          Stop helper
        </button>
      </div>

      <div className="settings-section-block">
        <div className="settings-section-heading">
          <div>
            <span className="section-label">Catalog</span>
            <h3>Find a track</h3>
          </div>
        </div>
        <div className="settings-inline-form">
          <label>
            <span>Storefront</span>
            <input onChange={(event) => setStorefront(event.target.value)} value={storefront} />
          </label>
          <label className="settings-grow">
            <span>Search</span>
            <input
              onChange={(event) => setSearchTerm(event.target.value)}
              onKeyDown={(event) => {
                if (event.key === 'Enter' && searchTerm.trim() && ready) search();
              }}
              placeholder="Artist, album, or track"
              value={searchTerm}
            />
          </label>
          <button
            className="pill primary"
            disabled={Boolean(busy) || !ready || !searchTerm.trim()}
            onClick={search}
            type="button"
          >
            Search
          </button>
        </div>

        {songs.length ? (
          <div className="settings-list">
            {songs.map((song) => (
              <article className="settings-list-row" key={String(song.song_id)}>
                <div>
                  <strong>{String(song.title || 'Untitled')}</strong>
                  <p>
                    {String(song.artist || 'Unknown artist')}
                    {song.album_title ? ` · ${String(song.album_title)}` : ''}
                  </p>
                </div>
                <div className="settings-actions">
                  <button
                    className="pill primary"
                    disabled={Boolean(busy) || !activeZoneID}
                    onClick={() => play(song)}
                    type="button"
                  >
                    Play
                  </button>
                  <button
                    className="pill"
                    disabled={Boolean(busy)}
                    onClick={() => queue(song, 'next')}
                    type="button"
                  >
                    Play next
                  </button>
                  <button
                    className="pill"
                    disabled={Boolean(busy)}
                    onClick={() => queue(song, 'end')}
                    type="button"
                  >
                    Add to queue
                  </button>
                </div>
              </article>
            ))}
          </div>
        ) : null}
      </div>
    </section>
  );
}

function StatusItem({ label, value }: { label: string; value: string }) {
  return (
    <div className="settings-summary-item">
      <span>{label}</span>
      <strong>{value}</strong>
    </div>
  );
}

function catalogSongSource(song: JsonRecord): SourceRef {
  return {
    kind: 'apple_music_track',
    song_id: String(song.song_id || ''),
    storefront: String(song.storefront || '') || null,
    title: String(song.title || '') || null,
    artist: String(song.artist || '') || null,
    album: String(song.album_title || '') || null,
    album_artist: String(song.album_artist || '') || null,
    album_id: String(song.album_id || '') || null,
    artwork_url: String(song.artwork_url || '') || null,
    duration_secs: numberValue(song.duration_secs),
    track_number: numberValue(song.track_number),
    disc_number: numberValue(song.disc_number),
    isrc: String(song.isrc || '') || null,
    radio: false,
    radio_context: null,
    playlist_context: null
  };
}

function recordArray(value: unknown): JsonRecord[] {
  return Array.isArray(value)
    ? value.filter((item): item is JsonRecord => Boolean(item) && typeof item === 'object')
    : [];
}

function numberValue(value: unknown): number | null {
  if (value === null || value === undefined || value === '') return null;
  const parsed = Number(value);
  return Number.isFinite(parsed) ? parsed : null;
}

function errorMessage(error: unknown): string {
  if (error && typeof error === 'object') {
    const record = error as JsonRecord;
    if (typeof record.message === 'string') return record.message;
    if (record.error && typeof record.error === 'object') {
      const nested = record.error as JsonRecord;
      if (typeof nested.message === 'string') return nested.message;
    }
  }
  return error instanceof Error ? error.message : String(error);
}

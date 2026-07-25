import { useCallback, useEffect, useState } from 'react';
import { endpoints } from '../../../shared/lib/api';
import { sourceRefToQueueItem } from '../../../shared/lib/queue';
import { routeToHash } from '../../../shared/lib/route';
import type { JsonRecord, QueueItem, ResolvedPlaySource, SourceRef } from '../../../shared/types';
import { Icon } from '../../../shared/ui/Icon';

type ScenarioName =
  | 'apple_apple'
  | 'local_apple'
  | 'qobuz_apple'
  | 'apple_local'
  | 'apple_qobuz'
  | 'mixed_run';

export function AppleMusicMvpPage({
  activeZoneStatus,
  addItemsToQueue
}: {
  activeZoneStatus: JsonRecord;
  addItemsToQueue: (items: QueueItem[], placement: 'next' | 'end') => Promise<boolean>;
}) {
  const [appleStatus, setAppleStatus] = useState<JsonRecord | null>(null);
  const [fozmoStatus, setFozmoStatus] = useState<JsonRecord | null>(activeZoneStatus);
  const [zoneQueue, setZoneQueue] = useState<JsonRecord | null>(null);
  const [busy, setBusy] = useState('');
  const [message, setMessage] = useState('');
  const [storefront, setStorefront] = useState('nz');
  const [searchTerm, setSearchTerm] = useState('');
  const [searchResult, setSearchResult] = useState<JsonRecord | null>(null);
  const [songID, setSongID] = useState('');
  const [albumID, setAlbumID] = useState('');
  const [catalogResult, setCatalogResult] = useState<JsonRecord | null>(null);
  const [catalogKind, setCatalogKind] = useState<'song' | 'album' | ''>('');
  const [scenarioQueue, setScenarioQueue] = useState<SourceRef[]>([]);
  const [rawQueue, setRawQueue] = useState('[]');
  const [selectedRow, setSelectedRow] = useState(0);
  const [localTrackID, setLocalTrackID] = useState('');
  const [qobuzTrackID, setQobuzTrackID] = useState('');
  const [scenarioName, setScenarioName] = useState<ScenarioName>('apple_apple');
  const captureConfirmed = true;
  const [seekSeconds, setSeekSeconds] = useState('30');
  const [localAlbumID, setLocalAlbumID] = useState('');
  const [albumPreview, setAlbumPreview] = useState<JsonRecord | null>(null);
  const [appleVersionID, setAppleVersionID] = useState('');
  const [resolvedPlan, setResolvedPlan] = useState<JsonRecord | null>(null);

  const processTap = recordValue(appleStatus?.process_tap);
  const playbackSession = recordValue(appleStatus?.playback_session);
  const helperPresent = appleStatus?.helper_present === true;
  const helperRunning = numberValue(appleStatus?.helper_pid) !== null;
  // Protocol v2 retains the old field name, but it now reports whether this
  // helper was signed with the MusicKit-enabled App ID's development profile.
  const musicKitProvisioned = appleStatus?.helper_musickit_entitled === true;
  const authorized = appleStatus?.authorization === 'authorized';
  const canPlayCatalog = appleStatus?.can_play_catalog_content === true;
  const activeZoneID = String(activeZoneStatus.active_zone_id || '');
  const activeZoneName = String(activeZoneStatus.active_zone_name || 'not selected');
  const activeZoneProtocol = String(activeZoneStatus.zone_protocol || '');
  const localZoneSupported = activeZoneProtocol
    ? activeZoneProtocol === 'local_core_audio'
    : activeZoneID === 'local-core';
  const scenarioNeedsCapture = scenarioQueue.some(
    (source) => normalizedSourceKind(source) === 'apple_music'
  );

  const loadState = useCallback(async () => {
    const [nextApple, nextFozmo] = await Promise.all([
      endpoints.appleMusicStatus(),
      activeZoneID ? endpoints.zoneStatus(activeZoneID) : endpoints.status()
    ]);
    const fozmo = nextFozmo as unknown as JsonRecord;
    setAppleStatus(nextApple);
    setFozmoStatus(fozmo);
    const zoneID = String(fozmo.active_zone_id || '');
    if (zoneID) {
      setZoneQueue((await endpoints.nowPlayingQueue(zoneID)) as unknown as JsonRecord);
    } else {
      setZoneQueue(null);
    }
    return nextApple;
  }, [activeZoneID]);

  useEffect(() => {
    loadState().catch((error) => setMessage(appleMusicErrorMessage(error)));
  }, [loadState]);

  useEffect(() => {
    const active = appleStatus?.playback_state === 'playing' || processTap?.state === 'running';
    const timer = window.setInterval(
      () => loadState().catch(() => undefined),
      active ? 1000 : 3000
    );
    return () => window.clearInterval(timer);
  }, [appleStatus?.playback_state, loadState, processTap?.state]);

  const run = async (key: string, action: () => Promise<unknown>, success: string) => {
    if (busy) return;
    setBusy(key);
    setMessage('');
    try {
      await action();
      await loadState();
      setMessage(success);
    } catch (error) {
      setMessage(appleMusicErrorMessage(error));
      await loadState().catch(() => undefined);
    } finally {
      setBusy('');
    }
  };

  const replaceScenario = (sources: SourceRef[], selected = 0) => {
    setScenarioQueue(sources);
    setRawQueue(JSON.stringify(sources, null, 2));
    setSelectedRow(Math.min(Math.max(selected, 0), Math.max(sources.length - 1, 0)));
  };

  const appendScenario = (source: SourceRef) => {
    replaceScenario([...scenarioQueue, source], selectedRow);
  };

  const lookupSong = () =>
    run(
      'lookup-song',
      async () => {
        const result = await endpoints.appleMusicCatalogSong(songID.trim(), storefront.trim());
        setCatalogResult(result);
        setCatalogKind('song');
      },
      'Normalized Apple Music song loaded.'
    );

  const searchCatalog = () =>
    run(
      'catalog-search',
      async () => {
        const result = await endpoints.appleMusicCatalogSearch(
          searchTerm.trim(),
          storefront.trim(),
          10
        );
        setSearchResult(result);
      },
      'Apple Music search complete.'
    );

  const playSearchResult = (song: JsonRecord) => {
    const source = catalogSongSource(song);
    setSongID(String(song.song_id || ''));
    return run(
      `play-search-${String(song.song_id || '')}`,
      () => endpoints.playAppleMusicScenario(activeZoneID, source, [], captureConfirmed),
      `${String(song.title || 'Apple Music song')} started on ${activeZoneName}.`
    );
  };

  const queueSearchResult = (song: JsonRecord, placement: 'next' | 'end') => {
    const source = catalogSongSource(song);
    const item = sourceRefToQueueItem(source);
    if (!item) {
      setMessage('This Apple Music result could not be converted into a queue item.');
      return;
    }
    return run(
      `queue-${placement}-${String(song.song_id || '')}`,
      async () => {
        await endpoints.confirmAppleMusicCapture();
        if (!(await addItemsToQueue([item], placement))) {
          throw new Error('Could not update the playback queue.');
        }
      },
      `${String(song.title || 'Apple Music song')} added ${
        placement === 'next' ? 'to play next' : 'to the end of the queue'
      }.`
    );
  };

  const lookupAlbum = () =>
    run(
      'lookup-album',
      async () => {
        const result = await endpoints.appleMusicCatalogAlbum(albumID.trim(), storefront.trim());
        setCatalogResult(result);
        setCatalogKind('album');
      },
      'Normalized Apple Music album and tracks loaded.'
    );

  const addCatalogResult = () => {
    if (!catalogResult) return;
    if (catalogKind === 'song') {
      appendScenario(catalogSongSource(catalogResult));
      return;
    }
    const tracks = recordArray(catalogResult.tracks).map(catalogSongSource);
    replaceScenario([...scenarioQueue, ...tracks], selectedRow);
  };

  const validateRawQueue = () => {
    try {
      const parsed = JSON.parse(rawQueue);
      if (!Array.isArray(parsed) || !parsed.every(isSourceRef)) {
        throw new Error('The editor must contain a SourceRef array.');
      }
      replaceScenario(parsed as SourceRef[], selectedRow);
      setMessage(`Validated ${parsed.length} scenario row${parsed.length === 1 ? '' : 's'}.`);
    } catch (error) {
      setMessage(appleMusicErrorMessage(error));
    }
  };

  const loadCannedScenario = () => {
    const local = localSource(validPositiveNumber(localTrackID) || 1);
    const qobuz = qobuzSource(validPositiveNumber(qobuzTrackID) || 1);
    const apple = appleSource(songID.trim() || '2037093408', storefront);
    const scenarios: Record<ScenarioName, SourceRef[]> = {
      apple_apple: [apple, { ...apple }],
      local_apple: [local, apple],
      qobuz_apple: [qobuz, apple],
      apple_local: [apple, local],
      apple_qobuz: [apple, qobuz],
      mixed_run: [local, apple, { ...apple }, qobuz]
    };
    replaceScenario(scenarios[scenarioName]);
  };

  const playScenario = () => {
    const source = scenarioQueue[selectedRow];
    if (!source) return;
    return run(
      'play-scenario',
      () =>
        endpoints.playAppleMusicScenario(
          activeZoneID,
          source,
          scenarioQueue.slice(selectedRow + 1),
          captureConfirmed
        ),
      `Scenario started from row ${selectedRow + 1} through the normal playback router.`
    );
  };

  const previewAlbumVersion = () =>
    run(
      'album-preview',
      async () => {
        const preview = await endpoints.appleMusicAlbumPreview(
          localAlbumID.trim(),
          albumID.trim(),
          storefront.trim()
        );
        setAlbumPreview(preview);
        const version = recordValue(preview.resulting_version);
        if (version?.id !== undefined) setAppleVersionID(String(version.id));
      },
      'Apple Music album-version match preview loaded.'
    );

  const linkAlbumVersion = () =>
    run(
      'album-link',
      async () => {
        const version = await endpoints.appleMusicAlbumLink(
          localAlbumID.trim(),
          albumID.trim(),
          storefront.trim()
        );
        setAppleVersionID(String(version.id || ''));
      },
      'Apple Music version linked to the local album.'
    );

  const resolveAlbumVersion = () =>
    run(
      'album-resolve',
      async () => {
        const plan = await endpoints.albumPlaySources(
          localAlbumID.trim(),
          0,
          false,
          validPositiveNumber(appleVersionID)
        );
        setResolvedPlan(plan as unknown as JsonRecord);
      },
      'Album playback plan resolved.'
    );

  const playResolvedVersion = () => {
    const sources = recordArray(resolvedPlan?.sources)
      .map((source) => resolvedToSourceRef(source as ResolvedPlaySource))
      .filter((source): source is SourceRef => source !== null);
    if (!sources.length) {
      setMessage('Resolve an Apple Music playback plan first.');
      return;
    }
    replaceScenario(sources);
    return run(
      'album-play',
      () =>
        endpoints.playAppleMusicScenario(
          activeZoneID,
          sources[0],
          sources.slice(1),
          captureConfirmed
        ),
      'Resolved Apple Music album version started through the normal router.'
    );
  };

  const currentSource = recordValue(fozmoStatus?.current_source);
  const helperNowPlaying = recordValue(appleStatus?.now_playing);
  const recentEvents = Array.isArray(appleStatus?.recent_events) ? appleStatus.recent_events : [];
  const searchAlbums = recordArray(searchResult?.albums);
  const searchSongs = recordArray(searchResult?.songs);

  return (
    <section className="settings-panel apple-music-capture-page apple-music-mvp-page">
      {message ? (
        <div className="metadata-assigner-message apple-music-message" role="status">
          {message}
        </div>
      ) : null}

      <div className="apple-music-mvp-banner">
        <div>
          <span className="section-label">Backend integration console</span>
          <h2>Apple Music</h2>
          <p>
            Inspect MusicKit catalog data, build mixed-provider queues, and exercise the same
            router, transport, history, DSP, and album-version paths used by the product.
          </p>
        </div>
        <span className={`stamp ${statusStampClass(appleStatus)}`}>{statusLabel(appleStatus)}</span>
      </div>

      <div className="settings-grid apple-music-grid">
        <ConsoleSection title="1 · Capability and authorization">
          <div className="settings-list compact-list">
            <StatusRow
              label="Helper"
              value={
                helperPresent
                  ? `${String(appleStatus?.helper_version || 'available')} · ${
                      helperRunning ? `PID ${String(appleStatus?.helper_pid)}` : 'not running'
                    }`
                  : 'missing'
              }
            />
            <StatusRow
              label="MusicKit App Service"
              value={
                musicKitProvisioned ? 'provisioned and enabled' : 'awaiting provisioned signing'
              }
            />
            <StatusRow
              label="Authorization"
              value={formatProtocolLabel(appleStatus?.authorization)}
            />
            <StatusRow
              label="Catalog playback"
              value={
                canPlayCatalog
                  ? 'available'
                  : appleStatus?.can_play_catalog_content === false
                    ? 'subscription unavailable'
                    : 'not checked'
              }
            />
            <StatusRow
              label="Active stream variant"
              value={
                appleStatus?.active_audio_variant
                  ? formatAudioVariant(appleStatus.active_audio_variant)
                  : 'not playing'
              }
            />
            <StatusRow
              label="Tap target"
              value={
                processTap?.target_pid
                  ? `${String(processTap.target_display_name || 'MusicKit helper')} · PID ${String(
                      processTap.target_pid
                    )}`
                  : 'not attached'
              }
            />
            <StatusRow
              label="Current zone"
              value={`${activeZoneName}${localZoneSupported ? ' · supported' : ' · local output required'}`}
            />
          </div>
          {!musicKitProvisioned ? (
            <div className="apple-music-routing-callout">
              <strong>Developer provisioning required</strong>
              <span>
                Helper launch, IPC, protocol, fake catalog/queue tests, schema migration, and UI are
                available now. MusicKit authorization requires a signed helper whose App ID has the
                MusicKit App Service enabled.
              </span>
            </div>
          ) : null}
          <div className="service-settings-actions">
            <button
              className="pill"
              type="button"
              disabled={Boolean(busy) || !helperPresent}
              onClick={() =>
                run('launch', endpoints.launchAppleMusicHelper, 'MusicKit helper launched.')
              }
            >
              Launch helper
            </button>
            <button
              className="pill"
              type="button"
              disabled={Boolean(busy) || !musicKitProvisioned}
              onClick={() =>
                run(
                  'authorize',
                  endpoints.authorizeAppleMusic,
                  'Apple Music authorization refreshed.'
                )
              }
            >
              {authorized ? 'Refresh authorization' : 'Authorize Apple Music'}
            </button>
            <button
              className="settings-heading-refresh"
              type="button"
              aria-label="Refresh Apple Music integration state"
              onClick={() =>
                loadState().catch((error) => setMessage(appleMusicErrorMessage(error)))
              }
            >
              <Icon path="M21 12a9 9 0 0 1-15.3 6.36M3 12A9 9 0 0 1 18.3 5.64M18 2v4h-4M6 22v-4h4" />
            </button>
          </div>
        </ConsoleSection>

        <ConsoleSection title="2 · Search and play">
          <form
            className="apple-music-catalog-search"
            onSubmit={(event) => {
              event.preventDefault();
              void searchCatalog();
            }}
          >
            <Field label="Search Apple Music albums and songs">
              <input
                className="input"
                type="search"
                value={searchTerm}
                maxLength={200}
                placeholder="Song, artist, or album"
                autoComplete="off"
                onChange={(event) => setSearchTerm(event.target.value)}
              />
            </Field>
            <button
              className="pill is-active"
              type="submit"
              disabled={Boolean(busy) || !searchTerm.trim()}
            >
              {busy === 'catalog-search' ? 'Searching…' : 'Search'}
            </button>
          </form>

          <div className="apple-music-search-target">
            <span>Playback target</span>
            <strong>{activeZoneName}</strong>
            <small>
              {localZoneSupported
                ? 'Tap a result to start it through the active DSP path.'
                : 'Apple Music currently requires an active local output zone.'}
            </small>
          </div>

          <label className="apple-music-capture-confirmation">
            <input
              type="checkbox"
              checked={captureConfirmed}
              readOnly
            />
            <span>
              Allow Fozmo to route native Music.app playback through the Fozmo Capture virtual
              driver and feed it through the selected local DSP/output path. Always on.
            </span>
          </label>

          <div className="apple-music-routing-callout">
            <strong>Lossless-only playback</strong>
            <span>
              Each selected track is restarted in Music.app at its decoder-log-verified ALAC rate,
              prebuffered, then released to the active local output. AAC, Dolby, Spatial Audio, and
              unknown formats are stopped before DSP handoff.
            </span>
          </div>

          {searchResult ? (
            <div className="apple-music-results" aria-label="Apple Music catalog search results">
              {searchAlbums.length ? (
                <>
                  <div className="apple-music-result-heading">Albums</div>
                  {searchAlbums.map((album, index) => {
                    const id = String(album.album_id || '');
                    const title = String(album.title || 'Untitled album');
                    const artist = String(album.artist || 'Unknown artist');
                    const artworkURL = String(album.artwork_url || '');
                    const releaseYear = String(album.release_date || '').slice(0, 4);
                    return (
                      <a
                        className="apple-music-result-row apple-music-search-result apple-music-album-search-result"
                        aria-label={`Open ${title} by ${artist}`}
                        href={routeToHash({
                          view: 'album',
                          id,
                          provider: 'apple_music',
                          storefront: String(album.storefront || storefront).trim()
                        })}
                        key={`${id}-${index}`}
                      >
                        <span className="apple-music-artwork" aria-hidden="true">
                          {artworkURL ? (
                            <img src={artworkURL} alt="" loading="lazy" />
                          ) : (
                            <Icon path="M4 6h16v12H4zM8 10h8M8 14h5" />
                          )}
                        </span>
                        <span className="apple-music-result-copy">
                          <strong>{title}</strong>
                          <small>{[artist, releaseYear].filter(Boolean).join(' · ')}</small>
                        </span>
                        <span className="apple-music-album-result-open">
                          Open album
                          <Icon path="m9 18 6-6-6-6" />
                        </span>
                      </a>
                    );
                  })}
                </>
              ) : null}
              {searchSongs.length ? (
                <>
                  <div className="apple-music-result-heading">Songs</div>
                  {searchSongs.map((song, index) => {
                    const songID = String(song.song_id || '');
                    const title = String(song.title || 'Untitled');
                    const artist = String(song.artist || 'Unknown artist');
                    const album = String(song.album_title || '');
                    const artworkURL = String(song.artwork_url || '');
                    const description = `${title} by ${artist}${album ? ` from ${album}` : ''}`;
                    const actionsDisabled =
                      Boolean(busy) || !captureConfirmed || !localZoneSupported;
                    return (
                      <div
                        className="apple-music-result-row apple-music-search-result"
                        key={`${songID}-${index}`}
                      >
                        <span className="apple-music-artwork" aria-hidden="true">
                          {artworkURL ? (
                            <img src={artworkURL} alt="" loading="lazy" />
                          ) : (
                            <Icon path="M9 18V5l12-2v13M9 18a3 3 0 1 1-2-2.83M21 16a3 3 0 1 1-2-2.83M9 9l12-2" />
                          )}
                        </span>
                        <span className="apple-music-result-copy">
                          <strong>{title}</strong>
                          <small>{[artist, album].filter(Boolean).join(' · ')}</small>
                        </span>
                        <span className="apple-music-result-actions">
                          <button
                            className="pill ghost apple-music-result-action"
                            type="button"
                            aria-label={`Play ${description} on ${activeZoneName}`}
                            disabled={actionsDisabled}
                            onClick={() => void playSearchResult(song)}
                          >
                            {busy === `play-search-${songID}` ? 'Starting…' : 'Play'}
                          </button>
                          <button
                            className="pill ghost apple-music-result-action"
                            type="button"
                            aria-label={`Play ${description} next`}
                            disabled={actionsDisabled}
                            onClick={() => void queueSearchResult(song, 'next')}
                          >
                            {busy === `queue-next-${songID}` ? 'Adding…' : 'Play next'}
                          </button>
                          <button
                            className="pill ghost apple-music-result-action"
                            type="button"
                            aria-label={`Add ${description} to the end of the queue`}
                            disabled={actionsDisabled}
                            onClick={() => void queueSearchResult(song, 'end')}
                          >
                            {busy === `queue-end-${songID}` ? 'Adding…' : 'Add to queue'}
                          </button>
                        </span>
                      </div>
                    );
                  })}
                </>
              ) : null}
              {!searchAlbums.length && !searchSongs.length ? (
                <p className="apple-music-empty-state">
                  No albums or songs matched “{String(searchResult.term || searchTerm)}”.
                </p>
              ) : null}
            </div>
          ) : (
            <p className="apple-music-empty-state">
              Search the Apple Music catalog by song, artist, or album.
            </p>
          )}

          <div className="apple-music-console-divider">
            <span>Catalog ID inspector</span>
          </div>
          <div className="apple-music-console-fields">
            <Field label="Storefront">
              <input
                className="input"
                value={storefront}
                maxLength={8}
                onChange={(event) => setStorefront(event.target.value.toLowerCase())}
              />
            </Field>
            <Field label="Song ID">
              <input
                className="input"
                value={songID}
                maxLength={256}
                placeholder="2037093408"
                onChange={(event) => setSongID(event.target.value)}
              />
            </Field>
            <Field label="Album ID">
              <input
                className="input"
                value={albumID}
                maxLength={256}
                placeholder="Apple Music album ID"
                onChange={(event) => setAlbumID(event.target.value)}
              />
            </Field>
          </div>
          <div className="service-settings-actions">
            <button
              className="pill"
              type="button"
              disabled={Boolean(busy) || !songID.trim()}
              onClick={lookupSong}
            >
              Lookup song
            </button>
            <button
              className="pill"
              type="button"
              disabled={Boolean(busy) || !albumID.trim()}
              onClick={lookupAlbum}
            >
              Lookup album
            </button>
            <button
              className="pill"
              type="button"
              disabled={!catalogResult}
              onClick={addCatalogResult}
            >
              Add {catalogKind === 'album' ? 'album tracks' : 'song'} to scenario
            </button>
          </div>
          <DebugJson title="Normalized catalog response" value={catalogResult} />
        </ConsoleSection>

        <ConsoleSection title="3 · Mixed queue scenario">
          <div className="apple-music-console-fields">
            <Field label="Local track ID">
              <div className="apple-music-inline-action">
                <input
                  className="input"
                  inputMode="numeric"
                  value={localTrackID}
                  onChange={(event) => setLocalTrackID(event.target.value)}
                />
                <button
                  className="pill"
                  type="button"
                  disabled={!validPositiveNumber(localTrackID)}
                  onClick={() => appendScenario(localSource(validPositiveNumber(localTrackID)))}
                >
                  Append
                </button>
              </div>
            </Field>
            <Field label="Qobuz track ID">
              <div className="apple-music-inline-action">
                <input
                  className="input"
                  inputMode="numeric"
                  value={qobuzTrackID}
                  onChange={(event) => setQobuzTrackID(event.target.value)}
                />
                <button
                  className="pill"
                  type="button"
                  disabled={!validPositiveNumber(qobuzTrackID)}
                  onClick={() => appendScenario(qobuzSource(validPositiveNumber(qobuzTrackID)))}
                >
                  Append
                </button>
              </div>
            </Field>
            <Field label="Apple Music song ID">
              <div className="apple-music-inline-action">
                <input
                  className="input"
                  value={songID}
                  onChange={(event) => setSongID(event.target.value)}
                />
                <button
                  className="pill"
                  type="button"
                  disabled={!songID.trim()}
                  onClick={() => appendScenario(appleSource(songID.trim(), storefront))}
                >
                  Append
                </button>
              </div>
            </Field>
          </div>

          <div className="apple-music-scenario-list" aria-label="Scenario queue">
            {scenarioQueue.length ? (
              scenarioQueue.map((source, index) => (
                <div className="apple-music-scenario-row" key={`${sourceKey(source)}-${index}`}>
                  <label>
                    <input
                      type="radio"
                      name="apple-music-start-row"
                      checked={selectedRow === index}
                      onChange={() => setSelectedRow(index)}
                    />
                    <strong>{index + 1}</strong>
                    <span>{sourceLabel(source)}</span>
                  </label>
                  <button
                    className="pill ghost"
                    type="button"
                    aria-label={`Remove scenario row ${index + 1}`}
                    onClick={() =>
                      replaceScenario(
                        scenarioQueue.filter((_, row) => row !== index),
                        Math.min(selectedRow, scenarioQueue.length - 2)
                      )
                    }
                  >
                    Remove
                  </button>
                </div>
              ))
            ) : (
              <p className="apple-music-empty-state">Append a source or load a canned scenario.</p>
            )}
          </div>

          <div className="apple-music-inline-action">
            <select
              className="input"
              aria-label="Canned mixed queue scenario"
              value={scenarioName}
              onChange={(event) => setScenarioName(event.target.value as ScenarioName)}
            >
              <option value="apple_apple">Apple → Apple</option>
              <option value="local_apple">Local → Apple</option>
              <option value="qobuz_apple">Qobuz → Apple</option>
              <option value="apple_local">Apple → Local</option>
              <option value="apple_qobuz">Apple → Qobuz</option>
              <option value="mixed_run">Local → Apple → Apple → Qobuz</option>
            </select>
            <button className="pill" type="button" onClick={loadCannedScenario}>
              Load canned scenario
            </button>
          </div>

          <details className="apple-music-diagnostics">
            <summary>Raw SourceRef[] editor</summary>
            <textarea
              className="input apple-music-json-editor"
              aria-label="Raw SourceRef queue JSON"
              value={rawQueue}
              spellCheck={false}
              onChange={(event) => setRawQueue(event.target.value)}
            />
            <div className="service-settings-actions">
              <button className="pill" type="button" onClick={validateRawQueue}>
                Validate JSON
              </button>
              <button className="pill" type="button" onClick={() => replaceScenario([])}>
                Clear
              </button>
            </div>
          </details>

          <div className="service-settings-actions">
            <button
              className="pill is-active"
              type="button"
              disabled={
                Boolean(busy) ||
                !scenarioQueue.length ||
                !localZoneSupported ||
                (scenarioNeedsCapture && !captureConfirmed)
              }
              onClick={playScenario}
            >
              Play from selected row
            </button>
          </div>
        </ConsoleSection>

        <ConsoleSection title="4 · Normal transport">
          <div className="service-settings-actions">
            <button
              className="pill"
              type="button"
              disabled={Boolean(busy)}
              onClick={() =>
                run('pause', () => endpoints.pauseZone(activeZoneID), 'Playback paused.')
              }
            >
              Pause
            </button>
            <button
              className="pill"
              type="button"
              disabled={Boolean(busy)}
              onClick={() =>
                run('resume', () => endpoints.resumeZone(activeZoneID), 'Playback resumed.')
              }
            >
              Resume
            </button>
            <button
              className="pill"
              type="button"
              disabled={Boolean(busy)}
              onClick={() =>
                run(
                  'next',
                  () => endpoints.nextZone(activeZoneID),
                  'Advanced through the normal queue.'
                )
              }
            >
              Next
            </button>
            <button
              className="pill service-settings-danger"
              type="button"
              disabled={Boolean(busy)}
              onClick={() =>
                run('stop', () => endpoints.stopZone(activeZoneID), 'Playback stopped.')
              }
            >
              Stop
            </button>
          </div>
          <div className="apple-music-inline-action">
            <input
              className="input"
              aria-label="Seek position in seconds"
              inputMode="decimal"
              value={seekSeconds}
              onChange={(event) => setSeekSeconds(event.target.value)}
            />
            <button
              className="pill"
              type="button"
              disabled={Boolean(busy) || numberValue(seekSeconds) === null}
              onClick={() =>
                run(
                  'seek',
                  () => endpoints.seekZone(activeZoneID, numberValue(seekSeconds) || 0),
                  `Seeked to ${seekSeconds} seconds.`
                )
              }
            >
              Seek
            </button>
          </div>
          <p className="apple-music-tap-rate-note">
            These buttons call <code>/api/pause</code>, <code>/api/resume</code>,{' '}
            <code>/api/next</code>, <code>/api/seek</code>, and <code>/api/stop</code>.
          </p>
        </ConsoleSection>

        <ConsoleSection title="5 · State inspection">
          <div className="apple-music-state-grid">
            <DebugJson title="Fozmo StatusResponse" value={fozmoStatus} />
            <DebugJson title="Current SourceRef" value={currentSource} />
            <DebugJson title="Persisted zone queue" value={zoneQueue} />
            <DebugJson
              title="Listening current + upcoming"
              value={{
                current_source: zoneQueue?.current_source || currentSource,
                queued_sources: zoneQueue?.queued_sources || []
              }}
            />
            <DebugJson title="Apple session revision + segment" value={playbackSession} />
            <DebugJson title="Helper now playing" value={helperNowPlaying} />
            <DebugJson title="Process-tap metrics" value={processTap} />
            <DebugJson title="Recent helper events" value={recentEvents} />
            <DebugJson title="Last transition error" value={appleStatus?.last_error || null} />
          </div>
        </ConsoleSection>

        <ConsoleSection title="6 · Album-version harness">
          <div className="apple-music-console-fields">
            <Field label="Local Fozmo album ID">
              <input
                className="input"
                inputMode="numeric"
                value={localAlbumID}
                onChange={(event) => setLocalAlbumID(event.target.value)}
              />
            </Field>
            <Field label="Apple Music album ID">
              <input
                className="input"
                value={albumID}
                onChange={(event) => setAlbumID(event.target.value)}
              />
            </Field>
            <Field label="Linked version ID">
              <input
                className="input"
                inputMode="numeric"
                value={appleVersionID}
                onChange={(event) => setAppleVersionID(event.target.value)}
              />
            </Field>
          </div>
          <div className="service-settings-actions">
            <button
              className="pill"
              type="button"
              disabled={!localAlbumID.trim() || !albumID.trim() || Boolean(busy)}
              onClick={previewAlbumVersion}
            >
              Preview match
            </button>
            <button
              className="pill"
              type="button"
              disabled={!localAlbumID.trim() || !albumID.trim() || Boolean(busy)}
              onClick={linkAlbumVersion}
            >
              Link as version
            </button>
            <button
              className="pill"
              type="button"
              disabled={!localAlbumID.trim() || Boolean(busy)}
              onClick={() =>
                run(
                  'album-unlink',
                  async () => {
                    await endpoints.appleMusicAlbumUnlink(localAlbumID.trim());
                    setAppleVersionID('');
                    setResolvedPlan(null);
                  },
                  'Apple Music version unlinked.'
                )
              }
            >
              Unlink
            </button>
            <button
              className="pill"
              type="button"
              disabled={
                !localAlbumID.trim() || !validPositiveNumber(appleVersionID) || Boolean(busy)
              }
              onClick={resolveAlbumVersion}
            >
              Resolve playback plan
            </button>
            <button
              className="pill is-active"
              type="button"
              disabled={!resolvedPlan || !captureConfirmed || Boolean(busy)}
              onClick={playResolvedVersion}
            >
              Play resolved Apple version
            </button>
          </div>
          <div className="apple-music-state-grid">
            <DebugJson title="Match preview" value={albumPreview} />
            <DebugJson title="Resolved playback plan" value={resolvedPlan} />
          </div>
        </ConsoleSection>

        <details className="settings-section-block apple-music-diagnostics">
          <summary className="settings-section-heading">
            <span className="section-label">Advanced diagnostics</span>
          </summary>
          <div className="panel raised apple-music-form-panel">
            <p>
              These controls bypass the normal router and remain available only for helper and
              Music.app process-tap diagnosis.
            </p>
            <div className="service-settings-actions">
              <button
                className="pill"
                type="button"
                disabled={!songID.trim() || Boolean(busy)}
                onClick={() =>
                  run(
                    'raw-play',
                    () => endpoints.playAppleMusicSong(songID.trim(), storefront.trim()),
                    'Raw helper playback started.'
                  )
                }
              >
                Raw helper play
              </button>
              <button
                className="pill"
                type="button"
                onClick={() =>
                  run('raw-pause', () => endpoints.controlAppleMusic('pause'), 'Raw helper paused.')
                }
              >
                Raw helper pause
              </button>
              <button
                className="pill"
                type="button"
                onClick={() =>
                  run(
                    'raw-resume',
                    () => endpoints.controlAppleMusic('resume'),
                    'Raw helper resumed.'
                  )
                }
              >
                Raw helper resume
              </button>
              <button
                className="pill"
                type="button"
                disabled={!captureConfirmed || Boolean(busy)}
                onClick={() =>
                  run(
                    'music-app-tap',
                    () => endpoints.startAppleMusicProcessTap(captureConfirmed, true),
                    'Music.app diagnostic process tap started.'
                  )
                }
              >
                Tap Music.app
              </button>
              <button
                className="pill"
                type="button"
                onClick={() =>
                  run(
                    'tap-stop',
                    endpoints.stopAppleMusicProcessTap,
                    'Diagnostic process tap stopped.'
                  )
                }
              >
                Stop diagnostic tap
              </button>
              <button
                className="pill service-settings-danger"
                type="button"
                disabled={!helperRunning || Boolean(busy)}
                onClick={() =>
                  run('shutdown', endpoints.shutdownAppleMusicHelper, 'MusicKit helper shut down.')
                }
              >
                Quit helper
              </button>
            </div>
          </div>
        </details>
      </div>
    </section>
  );
}

function ConsoleSection({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <section className="settings-section-block">
      <div className="settings-section-heading">
        <div className="section-label">{title}</div>
      </div>
      <div className="panel raised apple-music-form-panel">{children}</div>
    </section>
  );
}

function Field({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <label className="service-settings-field">
      <span>{label}</span>
      {children}
    </label>
  );
}

function StatusRow({ label, value }: { label: string; value: string }) {
  return (
    <div className="settings-kv">
      <span>{label}</span>
      <strong>{value}</strong>
    </div>
  );
}

function DebugJson({ title, value }: { title: string; value: unknown }) {
  return (
    <section className="apple-music-json-panel">
      <strong>{title}</strong>
      <pre>{JSON.stringify(value ?? null, null, 2)}</pre>
    </section>
  );
}

function localSource(trackID: number): SourceRef {
  return { kind: 'local_track', track_id: trackID };
}

function qobuzSource(trackID: number): SourceRef {
  return { kind: 'qobuz_track', track_id: trackID };
}

function appleSource(songID: string, storefront: string): SourceRef {
  return {
    kind: 'apple_music_track',
    song_id: songID,
    storefront: storefront.trim() || null
  };
}

function catalogSongSource(song: JsonRecord): SourceRef {
  return {
    kind: 'apple_music_track',
    song_id: String(song.song_id || ''),
    storefront: String(song.storefront || '') || null,
    title: stringOrNull(song.title),
    artist: stringOrNull(song.artist),
    album: stringOrNull(song.album_title),
    album_artist: stringOrNull(song.album_artist),
    album_id: stringOrNull(song.album_id),
    artwork_url: stringOrNull(song.artwork_url),
    duration_secs: numberValue(song.duration_secs),
    track_number: numberValue(song.track_number),
    disc_number: numberValue(song.disc_number),
    isrc: stringOrNull(song.isrc)
  };
}

function resolvedToSourceRef(source: ResolvedPlaySource): SourceRef | null {
  const kind = normalizedSourceKind(source);
  if (kind === 'local') {
    const trackID = numberValue(source.track_id);
    return trackID && trackID > 0 ? { ...source, kind: 'local_track', track_id: trackID } : null;
  }
  if (kind === 'qobuz') {
    const trackID = numberValue(source.track_id);
    return trackID && trackID > 0 ? { ...source, kind: 'qobuz_track', track_id: trackID } : null;
  }
  if (kind === 'apple_music' && source.song_id) {
    return {
      ...source,
      kind: 'apple_music_track',
      song_id: String(source.song_id),
      artwork_url: source.artwork_url || source.image_url || null
    };
  }
  return null;
}

function isSourceRef(value: unknown): value is SourceRef {
  const source = recordValue(value);
  if (!source) return false;
  const kind = normalizedSourceKind(source);
  if (kind === 'local' || kind === 'qobuz') {
    const trackID = numberValue(source.track_id);
    return trackID !== null && trackID > 0;
  }
  return kind === 'apple_music' && Boolean(String(source.song_id || '').trim());
}

function normalizedSourceKind(source: JsonRecord) {
  const kind = String(source.kind || '');
  if (kind === 'local' || kind === 'local_track') return 'local';
  if (kind === 'qobuz' || kind === 'qobuz_track') return 'qobuz';
  if (kind === 'apple_music' || kind === 'apple_music_track') return 'apple_music';
  return kind;
}

function sourceKey(source: SourceRef) {
  const kind = normalizedSourceKind(source);
  return kind === 'apple_music'
    ? `apple_music:${String(source.song_id || '')}`
    : `${kind}:${String(source.track_id || '')}`;
}

function sourceLabel(source: SourceRef) {
  const kind = normalizedSourceKind(source);
  const identity = kind === 'apple_music' ? source.song_id : source.track_id;
  const metadata = [source.artist, source.title].filter(Boolean).join(' · ');
  return `${formatProtocolLabel(kind)} · ${metadata || String(identity || 'invalid')}`;
}

function statusLabel(status: JsonRecord | null) {
  if (!status) return 'Checking';
  if (status.playback_state === 'playing') return 'Playing through Fozmo';
  if (status.helper_musickit_entitled !== true) return 'Awaiting provisioning';
  return formatProtocolLabel(status.state);
}

function statusStampClass(status: JsonRecord | null) {
  const state = String(status?.state || '');
  if (state === 'playing' || state === 'ready' || state === 'paused') return 'sage';
  if (state === 'failed' || state === 'helper_missing') return 'terra';
  return 'ochre';
}

function appleMusicErrorMessage(error: unknown) {
  const raw = error instanceof Error ? error.message : String(error);
  try {
    const parsed = JSON.parse(raw) as JsonRecord;
    return String(parsed.message || raw);
  } catch {
    return raw;
  }
}

function formatProtocolLabel(value: unknown) {
  return String(value || 'not available').replaceAll('_', ' ');
}

function formatAudioVariant(value: unknown) {
  switch (String(value || '')) {
    case 'lossless':
      return 'Lossless';
    case 'highResolutionLossless':
      return 'Hi-Res Lossless';
    case 'lossyStereo':
      return 'Lossy Stereo (AAC)';
    case 'dolbyAtmos':
      return 'Dolby Atmos';
    case 'dolbyAudio':
      return 'Dolby Audio';
    case 'spatialAudio':
      return 'Spatial Audio';
    default:
      return formatProtocolLabel(value);
  }
}

function recordValue(value: unknown): JsonRecord | null {
  return value && typeof value === 'object' && !Array.isArray(value) ? (value as JsonRecord) : null;
}

function recordArray(value: unknown) {
  return Array.isArray(value)
    ? value.map(recordValue).filter((item): item is JsonRecord => item !== null)
    : [];
}

function numberValue(value: unknown) {
  if (typeof value === 'string' && !value.trim()) return null;
  const number = Number(value);
  return Number.isFinite(number) ? number : null;
}

function validPositiveNumber(value: unknown) {
  const number = numberValue(value);
  return number !== null && Number.isInteger(number) && number > 0 ? number : 0;
}

function stringOrNull(value: unknown) {
  const string = String(value || '').trim();
  return string || null;
}

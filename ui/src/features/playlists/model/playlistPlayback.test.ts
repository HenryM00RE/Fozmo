import { describe, expect, it } from 'vitest';
import {
  albumTrackPlaybackMatchContext,
  albumTrackPlaybackState
} from '../../albums/components/AlbumTrackList';
import type { PlaybackStatus } from '../../playback/model/playbackStore';
import { normalizeQueueItem } from '../../../shared/lib/queue';
import type { QueueItem } from '../../../shared/types';
import { playbackFilenameOfTrack, playlistItemAsPlaybackTrack } from './playlistModel';

function activeIndex(items: QueueItem[], playbackStatus: PlaybackStatus) {
  const tracks = items.map(playlistItemAsPlaybackTrack);
  const contexts = {
    local: albumTrackPlaybackMatchContext({
      allTracks: tracks,
      isQobuz: false,
      playbackStatus,
      getPlaybackFilename: playbackFilenameOfTrack
    }),
    qobuz: albumTrackPlaybackMatchContext({
      allTracks: tracks,
      isQobuz: true,
      playbackStatus,
      getPlaybackFilename: playbackFilenameOfTrack
    })
  };
  return tracks.findIndex((track, index) =>
    albumTrackPlaybackState({
      track,
      playbackFilename: playbackFilenameOfTrack(track),
      context: items[index].qobuzTrack ? contexts.qobuz : contexts.local
    }).active
  );
}

function status(overrides: Partial<PlaybackStatus>): PlaybackStatus {
  return { state: 'Playing', ...overrides } as PlaybackStatus;
}

const normalize = (raw: unknown) => normalizeQueueItem(raw) as QueueItem;

describe('playlist row playback matching', () => {
  it('matches a Qobuz row, whose id lives on qobuzTrack and has no ref', () => {
    const items = [
      normalize({ qobuzTrack: { id: 111, title: 'The Flower Called Nowhere', artist: 'Stereolab', album: 'Dots And Loops' } }),
      normalize({ qobuzTrack: { id: 222, title: 'Les Fleurs', artist: 'Minnie Riperton', album: 'Come To My Garden' } })
    ];
    expect(items[1].ref).toBeUndefined();
    expect(
      activeIndex(items, status({ current_source: { kind: 'qobuz_track', track_id: 222 } }))
    ).toBe(1);
  });

  it('matches a local row resolved from a source ref that carries no file name', () => {
    const items = [
      normalize({ resolvedSource: { kind: 'local_track', track_id: 11, title: 'The Flower Called Nowhere', artist: 'Stereolab' } }),
      normalize({ resolvedSource: { kind: 'local_track', track_id: 22, title: 'Les Fleurs', artist: 'Minnie Riperton' } })
    ];
    // sourceRefToQueueItem falls back to the track id as the "filename"
    expect(items[1].filename).toBe('22');
    expect(playbackFilenameOfTrack(playlistItemAsPlaybackTrack(items[1]))).toBe('');
    expect(
      activeIndex(items, status({ current_source: { kind: 'local_track', track_id: 22 } }))
    ).toBe(1);
  });

  it('matches a local row on its real file name', () => {
    const items = [
      normalize({ title: 'One', artist: 'A', filename: '/music/one.flac', ref: { file_name: '/music/one.flac' } }),
      normalize({ title: 'Les Fleurs', artist: 'Minnie Riperton', filename: '/music/les-fleurs.flac', ref: { file_name: '/music/les-fleurs.flac' } })
    ];
    expect(activeIndex(items, status({ file_name: '/music/les-fleurs.flac' }))).toBe(1);
  });

  it('falls back to metadata when no id or file name is available', () => {
    const items = [
      normalize({ title: 'The Flower Called Nowhere', artist: 'Stereolab', album: 'Dots And Loops' }),
      normalize({ title: 'Les Fleurs', artist: 'Minnie Riperton', album: 'Come To My Garden' })
    ];
    expect(
      activeIndex(
        items,
        status({ track_title: 'Les Fleurs', track_artist: 'Minnie Riperton', track_album: 'Come To My Garden' })
      )
    ).toBe(1);
  });

  it('marks no row when nothing in the playlist is playing', () => {
    const items = [
      normalize({ qobuzTrack: { id: 111, title: 'The Flower Called Nowhere', artist: 'Stereolab' } })
    ];
    expect(activeIndex(items, status({ current_source: { kind: 'qobuz_track', track_id: 999 } }))).toBe(-1);
  });
});

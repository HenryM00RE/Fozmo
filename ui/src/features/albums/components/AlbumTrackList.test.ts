import { describe, expect, it } from 'vitest';
import type { LibraryTrack } from '../../../shared/types';
import type { PlaybackStatus } from '../../playback/model/playbackStore';
import { albumTrackPlaybackMatchContext, albumTrackPlaybackState } from './AlbumTrackList';

function playbackState(
  track: LibraryTrack,
  allTracks: LibraryTrack[],
  playbackStatus: PlaybackStatus
) {
  const context = albumTrackPlaybackMatchContext({
    allTracks,
    isQobuz: true,
    playbackStatus,
    getPlaybackFilename: (track) => `${track.artist || ''} - ${track.title || ''}`
  });
  return albumTrackPlaybackState({
    track,
    playbackFilename: `${track.artist || ''} - ${track.title || ''}`,
    context
  });
}

describe('AlbumTrackList playback matching', () => {
  it('marks a unique Qobuz row active from UPnP metadata when ids and filenames do not match', () => {
    const tracks: LibraryTrack[] = [
      {
        id: 101,
        title: 'Bye Bye Blackbird',
        artist: 'Patricia Barber',
        album: 'Nightclub'
      },
      {
        id: 102,
        title: 'Invitation',
        artist: 'Patricia Barber',
        album: 'Nightclub'
      }
    ];
    const status: PlaybackStatus = {
      state: 'Playing',
      file_name: 'Bye Bye Blackbird',
      current_source: {
        kind: 'qobuz_track',
        track_id: 999
      },
      track_title: 'Bye Bye Blackbird',
      track_artist: 'Patricia Barber',
      track_album: 'Nightclub'
    };

    expect(playbackState(tracks[0], tracks, status)).toEqual({ active: true, playing: true });
    expect(playbackState(tracks[1], tracks, status)).toEqual({ active: false, playing: false });
  });

  it('does not mark duplicate Qobuz metadata matches active', () => {
    const tracks: LibraryTrack[] = [
      {
        id: 201,
        title: 'Intro',
        artist: 'The Band',
        album: 'The Album'
      },
      {
        id: 202,
        title: 'Intro',
        artist: 'The Band',
        album: 'The Album'
      }
    ];
    const status: PlaybackStatus = {
      state: 'Playing',
      file_name: 'Intro',
      current_source: {
        kind: 'qobuz_track',
        track_id: 999
      },
      track_title: 'Intro',
      track_artist: 'The Band',
      track_album: 'The Album'
    };

    expect(playbackState(tracks[0], tracks, status)).toEqual({ active: false, playing: false });
    expect(playbackState(tracks[1], tracks, status)).toEqual({ active: false, playing: false });
  });
});

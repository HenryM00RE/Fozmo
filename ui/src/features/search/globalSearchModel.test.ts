import { describe, expect, it } from 'vitest';
import type { GlobalSearchState } from '../../shared/lib/appSupport';
import type { LibraryAlbum, LibraryTrack, QobuzTrack } from '../../shared/types';
import { buildGlobalSearchView } from './globalSearchModel';

const noop = () => undefined;

function searchView(query: string, results: GlobalSearchState) {
  return buildGlobalSearchView({
    albums: results.local.albums,
    onAddTrackToPlaylist: noop,
    onClose: noop,
    onOpenAlbum: noop,
    onOpenArtist: noop,
    onOpenQobuzAlbum: noop,
    onPlayQobuz: noop,
    onPlayTrack: noop,
    onQueueAlbum: noop,
    onQueueTrack: noop,
    query,
    results,
    showAll: true
  });
}

function visibleTitles(query: string, results: GlobalSearchState) {
  const view = searchView(query, results);
  return [...(view.topResult ? [view.topResult] : []), ...view.rows].map((row) => row.title);
}

function emptyResults(): GlobalSearchState {
  return {
    local: { songs: [], albums: [], artists: [] },
    qobuz: { songs: [], albums: [], artists: [] },
    localLoading: false,
    qobuzLoading: false,
    localError: null,
    qobuzError: null
  };
}

function song(id: number, title: string, artist: string, album: string): LibraryTrack {
  return { id, title, artist, album, album_artist: artist, duration_secs: 240 };
}

function qobuzSong(id: number, title: string, artist: string, album: string): QobuzTrack {
  return { id, title, artist, album, album_id: `q-${id}`, duration: 240 };
}

function album(
  id: number | string,
  title: string,
  artist: string,
  year = 2000,
  trackCount = 10
): LibraryAlbum {
  return { id, title, album_artist: artist, year, track_count: trackCount };
}

describe('buildGlobalSearchView ranking', () => {
  it('uses the songs-page action order and adds artist navigation for song results', () => {
    const results = emptyResults();
    results.local.albums = [album(7, 'TNT', 'Tortoise', 1998, 12)];
    results.local.songs = [{ ...song(1, 'Ten-Day Interval', 'Tortoise', 'TNT'), album_id: 7 }];

    const view = searchView('Ten-Day Interval', results);

    expect(view.topResult?.actions?.map((action) => action.id)).toEqual([
      'play',
      'add-next',
      'add-to-playlist',
      'go-to-album',
      'go-to-artist',
      'add-to-queue'
    ]);
  });

  it('keeps loading status text empty so the UI can use stable skeleton rows', () => {
    const results = emptyResults();
    results.localLoading = true;
    results.qobuzLoading = true;

    const view = searchView('Tortoise', results);

    expect(view.isLoading).toBe(true);
    expect(view.status).toBe('');
  });

  it('prefers the in-library Thom Yorke artist and artist-owned records over broad featured hits', () => {
    const results = emptyResults();
    results.local.artists = [{ name: 'Thom Yorke', album_count: 4, track_count: 50 }];
    results.qobuz.artists = [
      { name: 'Thom Yorke', image_url: 'thom.jpg', albums_count: 4 },
      { name: 'Thom Yorke Colin Greenwood Jonny Greenwood Philip Selway' }
    ];
    results.local.albums = [
      album(1, 'ANIMA', 'Thom Yorke', 2019, 9),
      album(2, 'The Eraser', 'Thom Yorke', 2006, 9)
    ];
    results.local.songs = [song(1, 'Traffic', 'Thom Yorke', 'ANIMA')];
    results.qobuz.songs = [qobuzSong(11, 'Traffic Lights (feat. Thom Yorke)', 'Flea', 'Honora')];

    const view = searchView('thom yorke', results);
    expect(view.topResult?.title).toBe('Thom Yorke');
    const titles = visibleTitles('thom yorke', results);
    expect(titles.indexOf('ANIMA')).toBeLessThan(
      titles.indexOf('Traffic Lights (feat. Thom Yorke)')
    );
    expect(titles.indexOf('The Eraser')).toBeLessThan(
      titles.indexOf('Thom Yorke Colin Greenwood Jonny Greenwood Philip Selway')
    );
  });

  it('treats Weird Fishes as a song search instead of promoting weak catalog artists', () => {
    const results = emptyResults();
    results.local.songs = [song(1, 'Weird Fishes/Arpeggi', 'Radiohead', 'In Rainbows')];
    results.qobuz.artists = [{ name: 'Weird Fishes' }];
    results.qobuz.albums = [
      album('q1', 'Weird Fishes', 'Lianne La Havas', 2020, 1),
      album('q2', 'Weird Fishes', 'Dune Moss', 2025, 1)
    ];
    results.qobuz.songs = [qobuzSong(2, 'Weird Fishes', 'Lianne La Havas', 'Weird Fishes')];

    const view = searchView('weird fishes', results);
    expect(view.topResult?.kind).toBe('song');
    expect(view.topResult?.title).toBe('Weird Fishes/Arpeggi');
    const titles = visibleTitles('weird fishes', results);
    expect(titles.indexOf('Weird Fishes/Arpeggi')).toBeLessThan(titles.indexOf('Weird Fishes'));
  });

  it('keeps In Rainbows album versions and tracks ahead of tribute catalog results', () => {
    const results = emptyResults();
    results.local.albums = [
      album(1, 'In Rainbows', 'Radiohead', 2007, 10),
      album(2, 'In Rainbows (Disk 2)', 'Radiohead', 2007, 8)
    ];
    results.local.songs = [
      song(1, '15 Step', 'Radiohead', 'In Rainbows'),
      song(2, 'Bodysnatchers', 'Radiohead', 'In Rainbows')
    ];
    results.qobuz.albums = [
      album(
        'q1',
        "Vitamin String Quartet Performs Radiohead's In Rainbows",
        'Vitamin String Quartet',
        2009,
        10
      )
    ];

    const view = searchView('In Rainbows', results);
    expect(view.topResult?.kind).toBe('album');
    expect(view.topResult?.title).toBe('In Rainbows');
    const titles = visibleTitles('In Rainbows', results);
    expect(titles.indexOf('15 Step')).toBeLessThan(
      titles.indexOf("Vitamin String Quartet Performs Radiohead's In Rainbows")
    );
  });

  it('accent-folds Bjork and ranks direct Björk library items before other artists and featured songs', () => {
    const results = emptyResults();
    results.local.artists = [{ name: 'Björk', album_count: 2, track_count: 20 }];
    results.local.albums = [album(1, 'Homogenic', 'Björk', 1997, 10)];
    results.local.songs = [song(1, 'Bachelorette', 'Björk', 'Homogenic')];
    results.qobuz.artists = [
      { name: 'Brant Bjork', image_url: 'brant.jpg' },
      { name: 'Hera Björk', image_url: 'hera.jpg' }
    ];
    results.qobuz.songs = [qobuzSong(2, 'Berghain (feat. Björk & Yves Tumor)', 'ROSALÍA', 'LUX')];

    const view = searchView('Bjork', results);
    expect(view.topResult?.title).toBe('Björk');
    const titles = visibleTitles('Bjork', results);
    expect(titles.indexOf('Homogenic')).toBeLessThan(
      titles.indexOf('Berghain (feat. Björk & Yves Tumor)')
    );
    expect(titles.indexOf('Bachelorette')).toBeLessThan(titles.indexOf('Brant Bjork'));
  });

  it('uses exact artist intent so Atoms For Peace albums beat exact-title covers by other artists', () => {
    const results = emptyResults();
    results.local.artists = [{ name: 'Atoms For Peace', album_count: 2, track_count: 31 }];
    results.local.albums = [album(1, 'Amok', 'Atoms For Peace', 2013, 9)];
    results.local.songs = [song(1, 'Default', 'Atoms For Peace', 'Amok')];
    results.qobuz.songs = [qobuzSong(2, 'Atoms For Peace', 'Thom Yorke', 'Auckland')];
    results.qobuz.albums = [
      album('q1', 'Atoms for Peace', 'Tom Caufield', 2024, 1),
      album('q2', 'Atoms For Peace', 'KID RVA', 2024, 1)
    ];

    const view = searchView('Atoms for peace', results);
    expect(view.topResult?.title).toBe('Atoms For Peace');
    const titles = visibleTitles('Atoms for peace', results);
    expect(titles.indexOf('Amok')).toBeLessThan(titles.indexOf('Atoms for Peace'));
    expect(titles.indexOf('Default')).toBeLessThan(titles.indexOf('Atoms For Peace', 1));
  });
});

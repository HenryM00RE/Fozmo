import { orderAlbumTracks, positiveNumber, titleOf } from '../../../shared/lib/appSupport';
import type { LibraryAlbum, LibraryTrack } from '../../../shared/types';

export type EditableAlbumTrack = LibraryTrack & {
  editorKey: string;
};

export function editableAlbumTracks(tracks: LibraryTrack[]) {
  return orderAlbumTracks(tracks).map((track, index) => ({
    ...track,
    disc_number: positiveNumber(track.disc_number) || 1,
    editorKey: String(track.id ?? track.track_id ?? track.file_name ?? index)
  }));
}

export function moveAlbumTrack(tracks: EditableAlbumTrack[], fromIndex: number, toIndex: number) {
  if (
    fromIndex === toIndex ||
    fromIndex < 0 ||
    toIndex < 0 ||
    fromIndex >= tracks.length ||
    toIndex >= tracks.length
  ) {
    return tracks;
  }
  const next = [...tracks];
  const [moved] = next.splice(fromIndex, 1);
  next.splice(toIndex, 0, moved);
  return next;
}

export function updateAlbumTrackTitle(
  tracks: EditableAlbumTrack[],
  editorKey: string,
  title: string
) {
  return tracks.map((track) => (track.editorKey === editorKey ? { ...track, title } : track));
}

export function updateAlbumTrackDisc(
  tracks: EditableAlbumTrack[],
  editorKey: string,
  discNumber: number | null
) {
  return tracks.map((track) =>
    track.editorKey === editorKey ? { ...track, disc_number: discNumber } : track
  );
}

export function albumEditorInitialTitle(album: LibraryAlbum | null) {
  return titleOf(album, '');
}

export function albumEditorInitialArtist(album: LibraryAlbum | null) {
  return String(album?.album_artist || album?.artist || '');
}

export function albumEditorInitialYear(album: LibraryAlbum | null) {
  const year = Number(album?.year);
  return Number.isFinite(year) && year > 0 ? String(Math.trunc(year)) : '';
}

export function albumEditorYearPayload(year: string) {
  const trimmed = year.trim();
  if (!trimmed) return null;
  const numeric = Number(trimmed);
  return Number.isFinite(numeric) ? Math.trunc(numeric) : null;
}

export function trackEditPayload(tracks: EditableAlbumTrack[]) {
  const discSlots = discSlotPlan(tracks);
  return tracks
    .map((track, index) => {
      const slot = discSlots[index] || { disc: 1, track: index + 1 };
      return {
        id: Number(track.id ?? track.track_id),
        title: titleOf(track, '').trim() || 'Untitled track',
        artist: track.artist ? String(track.artist) : null,
        track_number: slot.track,
        disc_number: slot.disc
      };
    })
    .filter((track) => Number.isFinite(track.id));
}

function discSlotPlan(tracks: EditableAlbumTrack[]) {
  const counts = new Map<number, number>();
  return tracks.map((track) => {
    const disc = positiveNumber(track.disc_number) || 1;
    const trackNumber = (counts.get(disc) || 0) + 1;
    counts.set(disc, trackNumber);
    return { disc, track: trackNumber };
  });
}

export function albumEditorHasChanges(
  album: LibraryAlbum | null,
  originalTracks: EditableAlbumTrack[],
  editedTracks: EditableAlbumTrack[],
  title: string,
  albumArtist: string,
  year: string,
  selectedQobuzId: string | null,
  hasCustomCover = false
) {
  if (
    albumEditorMetadataHasChanges(album, originalTracks, editedTracks, title, albumArtist, year)
  ) {
    return true;
  }
  if (selectedQobuzId) return true;
  return hasCustomCover;
}

export function albumEditorMetadataHasChanges(
  album: LibraryAlbum | null,
  originalTracks: EditableAlbumTrack[],
  editedTracks: EditableAlbumTrack[],
  title: string,
  albumArtist: string,
  year: string
) {
  if (title.trim() !== albumEditorInitialTitle(album).trim()) return true;
  if (albumArtist.trim() !== albumEditorInitialArtist(album).trim()) return true;
  if (year.trim() !== albumEditorInitialYear(album)) return true;
  if (originalTracks.length !== editedTracks.length) return true;
  return originalTracks.some(
    (track, index) =>
      track.editorKey !== editedTracks[index]?.editorKey ||
      titleOf(track, 'Untitled track').trim() !==
        titleOf(editedTracks[index], 'Untitled track').trim() ||
      (positiveNumber(track.disc_number) || 1) !==
        (positiveNumber(editedTracks[index]?.disc_number) || 1)
  );
}

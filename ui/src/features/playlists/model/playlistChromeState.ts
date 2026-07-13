import type { Playlist, QueueItem } from '../../../shared/types';
import type { PlaylistPickerState } from '../components/PlaylistPicker';

export type PlaylistChromeState = {
  onAddToPlaylist: (playlist: Playlist, items: QueueItem[]) => Promise<boolean>;
  onClosePlaylistPicker: () => void;
  onCreatePlaylist: (name: string, items: QueueItem[]) => Promise<boolean>;
  picker: PlaylistPickerState;
  playlists: Playlist[];
};

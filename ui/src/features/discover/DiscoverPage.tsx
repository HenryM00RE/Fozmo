import type { JsonRecord } from '../../shared/types';
import { SetupNotice } from '../../shared/ui/SetupNotice';
import {
  HomeQobuzPlaylists,
  HomeQobuzSections,
  hasVisibleHomeQobuzSections,
  QobuzHomeSkeleton
} from '../home/components/HomeQobuzSections';

type DiscoverPageProps = {
  loading: boolean;
  onOpenServices: () => void;
  onOpenArtist: (name: string) => void;
  onOpenQobuzAlbum: (id: string | number) => void;
  onOpenQobuzPlaylist: (id: string | number) => void;
  onPlayQobuzAlbum: (id: string | number) => void;
  onPlayQobuzPlaylist: (id: string | number) => void;
  onToggleQobuzAlbumSelection: (album: JsonRecord) => void;
  onToggleQobuzPlaylistSelection: (playlist: JsonRecord) => void;
  qobuzHome: JsonRecord | null;
  qobuzConnected: boolean;
  selectedKeys: Set<string>;
  selectionActive: boolean;
};

export function DiscoverPage({
  loading,
  onOpenServices,
  onOpenArtist,
  onOpenQobuzAlbum,
  onOpenQobuzPlaylist,
  onPlayQobuzAlbum,
  onPlayQobuzPlaylist,
  onToggleQobuzAlbumSelection,
  onToggleQobuzPlaylistSelection,
  qobuzHome,
  qobuzConnected,
  selectedKeys,
  selectionActive
}: DiscoverPageProps) {
  return (
    <section className="view discover-view">
      <div className="library-page-heading">
        <div>
          <h1>Discover</h1>
        </div>
      </div>
      {!qobuzConnected ? (
        <SetupNotice
          actionLabel="Link Qobuz"
          message="Link your Qobuz account to browse recommendations, new releases and playlists."
          onAction={onOpenServices}
        />
      ) : null}
      {qobuzConnected ? (
        <>
          <HomeQobuzSections
            qobuzHome={qobuzHome}
            selectedKeys={selectedKeys}
            selectionActive={selectionActive}
            onOpenQobuzAlbum={onOpenQobuzAlbum}
            onPlayQobuzAlbum={onPlayQobuzAlbum}
            onToggleQobuzAlbumSelection={onToggleQobuzAlbumSelection}
            onOpenArtist={onOpenArtist}
          />
          <HomeQobuzPlaylists
            qobuzHome={qobuzHome}
            selectedKeys={selectedKeys}
            selectionActive={selectionActive}
            onOpenQobuzPlaylist={onOpenQobuzPlaylist}
            onPlayQobuzPlaylist={onPlayQobuzPlaylist}
            onToggleQobuzPlaylistSelection={onToggleQobuzPlaylistSelection}
          />
          {!hasVisibleHomeQobuzSections(qobuzHome) && loading ? (
            <section className="library-section home-qobuz-section">
              <QobuzHomeSkeleton />
            </section>
          ) : null}
        </>
      ) : null}
    </section>
  );
}

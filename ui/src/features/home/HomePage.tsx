import { useState } from 'react';
import type { JsonRecord, Playlist } from '../../shared/types';
import { SetupNotice } from '../../shared/ui/SetupNotice';
import { NewOnQobuzSection, qobuzNewReleaseAlbums } from './components/HomeQobuzSections';
import { RecentlyPlayedSection } from './components/RecentlyPlayedSection';

export function HomePage({
  loading,
  recent,
  recentLoading,
  playlists,
  qobuzHome,
  selectedKeys,
  selectionActive,
  onOpenRecent,
  onPlayRecent,
  onToggleRecentSelection,
  onToggleQobuzAlbumSelection,
  onOpenQobuzAlbum,
  onPlayQobuzAlbum,
  qobuzConnected,
  onOpenServices
}: {
  loading: boolean;
  recent: JsonRecord[];
  recentLoading: boolean;
  playlists: Playlist[];
  qobuzHome: JsonRecord | null;
  qobuzConnected: boolean;
  selectedKeys: Set<string>;
  selectionActive: boolean;
  onOpenRecent: (item: JsonRecord) => void;
  onPlayRecent: (item: JsonRecord) => void;
  onToggleRecentSelection: (item: JsonRecord) => void;
  onToggleQobuzAlbumSelection: (album: JsonRecord) => void;
  onOpenQobuzAlbum: (id: string | number) => void;
  onPlayQobuzAlbum: (id: string | number) => void;
  onOpenServices: () => void;
}) {
  const [expanded, setExpanded] = useState(false);
  const [newOnQobuzExpanded, setNewOnQobuzExpanded] = useState(false);
  return (
    <section className="view home-view">
      <div className="home-hero">
        <div>
          <h1>Home</h1>
        </div>
      </div>
      <RecentlyPlayedSection
        expanded={expanded}
        onExpandedChange={setExpanded}
        onOpenRecent={onOpenRecent}
        onPlayRecent={onPlayRecent}
        onToggleRecentSelection={onToggleRecentSelection}
        playlists={playlists}
        recent={recent}
        loading={recentLoading}
        selectedKeys={selectedKeys}
        selectionActive={selectionActive}
      />
      {!qobuzConnected ? (
        <SetupNotice
          actionLabel="Link Qobuz"
          message="Link your Qobuz account to start streaming and see personalised music here."
          onAction={onOpenServices}
        />
      ) : null}
      <NewOnQobuzSection
        expanded={newOnQobuzExpanded}
        loading={qobuzConnected && loading && !qobuzNewReleaseAlbums(qobuzHome).length}
        qobuzHome={qobuzHome}
        selectedKeys={selectedKeys}
        selectionActive={selectionActive}
        onExpandedChange={setNewOnQobuzExpanded}
        onOpenQobuzAlbum={onOpenQobuzAlbum}
        onPlayQobuzAlbum={onPlayQobuzAlbum}
        onToggleQobuzAlbumSelection={onToggleQobuzAlbumSelection}
      />
    </section>
  );
}

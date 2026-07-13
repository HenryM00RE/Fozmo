import type { JsonRecord } from '../../../shared/types';
import {
  formatListeningTime,
  type HistoryTarget,
  historyRecentKey,
  historyRecentTrackTarget
} from '../model/historyModel';
import { HistoryArtwork } from './HistoryArtwork';

const RECENT_TRACK_PREVIEW_LIMIT = 10;

export function HistoryRecentTracks({
  onOpenAll,
  onOpenTarget,
  recentTracks
}: {
  onOpenAll?: () => void;
  onOpenTarget: (target: HistoryTarget | null) => void;
  recentTracks: JsonRecord[];
}) {
  const previewTracks = recentTracks.slice(0, RECENT_TRACK_PREVIEW_LIMIT);

  return (
    <section className="history-recent-panel">
      <div className="panel-heading">
        <div>
          <div className="section-label">History</div>
          <h2>Recent tracks</h2>
        </div>
        {recentTracks.length > previewTracks.length && onOpenAll ? (
          <button className="history-all-button" type="button" onClick={onOpenAll}>
            All
          </button>
        ) : null}
      </div>
      <div className="history-recent-list">
        {previewTracks.length ? (
          previewTracks.map((entry, index) => (
            <HistoryRecentTrackRow
              entry={entry}
              key={historyRecentKey(entry, index)}
              onOpenTarget={onOpenTarget}
            />
          ))
        ) : (
          <div className="history-empty">No plays yet.</div>
        )}
      </div>
    </section>
  );
}

function HistoryRecentTrackRow({
  entry,
  onOpenTarget
}: {
  entry: JsonRecord;
  onOpenTarget: (target: HistoryTarget | null) => void;
}) {
  const source = (entry.source || {}) as JsonRecord;
  const title = String(entry.title || source.title || 'Unknown track');
  const artist = String(entry.artist || source.artist || 'Unknown artist');
  const target = historyRecentTrackTarget(entry);

  return (
    <button
      className="history-recent-row history-click-row"
      type="button"
      onClick={() => onOpenTarget(target)}
    >
      <div className="history-rank-art">
        <HistoryArtwork item={{ ...source, ...entry }} />
      </div>
      <div className="history-rank-text">
        <strong title={title}>{title}</strong>
        <span>{artist}</span>
      </div>
      <em>{formatListeningTime(entry.played_secs)}</em>
    </button>
  );
}

import type { JsonRecord } from '../../../shared/types';
import {
  formatListeningTime,
  HISTORY_RANK_VISIBLE_LIMIT,
  type HistoryRankKind,
  type HistoryTarget,
  historyRankKey,
  historyRankTarget
} from '../model/historyModel';
import { ArtistAvatar, HistoryArtwork } from './HistoryArtwork';

export function HistoryRankPanel({
  title,
  items,
  kind = 'default',
  onOpenAll,
  onOpenTarget
}: {
  title: string;
  items: JsonRecord[];
  kind?: HistoryRankKind;
  onOpenAll?: () => void;
  onOpenTarget: (target: HistoryTarget | null) => void;
}) {
  const visibleItems = items.slice(0, HISTORY_RANK_VISIBLE_LIMIT);
  return (
    <section className="history-rank-panel">
      <div className="history-panel-head">
        <h2>{title}</h2>
        {items.length && onOpenAll ? (
          <button className="history-all-button" type="button" onClick={onOpenAll}>
            All
          </button>
        ) : null}
      </div>
      <div className="history-rank-list">
        {visibleItems.length ? (
          visibleItems.map((item, index) => (
            <HistoryRankRow
              item={item}
              index={index}
              kind={kind}
              key={historyRankKey(item, index)}
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

function HistoryRankRow({
  item,
  index,
  kind,
  onOpenTarget
}: {
  item: JsonRecord;
  index: number;
  kind: HistoryRankKind;
  onOpenTarget: (target: HistoryTarget | null) => void;
}) {
  const isArtist = kind === 'artist';
  const isSong = kind === 'song';
  const name = String(item.name || item.title || item.album || 'Unknown');
  const playLabel = `${Number(item.play_count || 0)} plays`;
  const subtitle =
    isSong && item.subtitle
      ? `${String(item.subtitle)} - ${playLabel}`
      : String(item.subtitle || item.artist || item.album_artist || playLabel);
  const target = historyRankTarget(item, kind);

  return (
    <button
      className="history-rank-row history-click-row"
      type="button"
      onClick={() => onOpenTarget(target)}
    >
      <span className="history-rank-index">{index + 1}</span>
      <div className={`history-rank-art${isArtist ? ' history-artist-avatar' : ''}`}>
        {isArtist ? <ArtistAvatar name={name} /> : <HistoryArtwork item={item} />}
      </div>
      <div className="history-rank-text">
        <strong title={name}>{name}</strong>
        <span>{subtitle}</span>
      </div>
      {isSong ? null : <em>{formatListeningTime(item.listened_secs)}</em>}
    </button>
  );
}

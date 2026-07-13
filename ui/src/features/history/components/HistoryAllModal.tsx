import { createPortal } from 'react-dom';
import type { JsonRecord } from '../../../shared/types';
import { Icon } from '../../../shared/ui/Icon';
import { Modal } from '../../../shared/ui/Modal';
import {
  formatListeningTime,
  type HistoryRankKind,
  type HistoryTarget,
  historyRankKey,
  historyRankTarget,
  historyRecentKey,
  historyRecentTrackTarget
} from '../model/historyModel';
import { ArtistAvatar, HistoryArtwork } from './HistoryArtwork';

type HistoryAllModalProps = {
  items: JsonRecord[];
  kind: HistoryRankKind | 'recent';
  onClose: () => void;
  onOpenTarget: (target: HistoryTarget | null) => void;
  title: string;
};

export function HistoryAllModal({
  items,
  kind,
  onClose,
  onOpenTarget,
  title
}: HistoryAllModalProps) {
  const modalTitleId = 'history-all-modal-title';
  const portalHost = typeof document === 'undefined' ? null : document.querySelector('.react-app');

  const modal = (
    <Modal open className="history-all-backdrop" ariaLabelledBy={modalTitleId} onClose={onClose}>
      <div className="history-all-panel">
        <div className="history-all-head">
          <div>
            <div className="section-label">History</div>
            <h2 id={modalTitleId}>{title}</h2>
          </div>
          <button
            className="history-all-close"
            type="button"
            title="Close"
            aria-label="Close"
            onClick={onClose}
          >
            <Icon path="M18 6 6 18M6 6l12 12" />
          </button>
        </div>
        <div className="history-all-list">
          {items.length ? (
            items.map((item, index) => (
              <HistoryAllRow
                item={item}
                index={index}
                kind={kind}
                key={
                  kind === 'recent' ? historyRecentKey(item, index) : historyRankKey(item, index)
                }
                onOpenTarget={(target) => {
                  onClose();
                  onOpenTarget(target);
                }}
              />
            ))
          ) : (
            <div className="history-empty">No plays yet.</div>
          )}
        </div>
      </div>
    </Modal>
  );

  return portalHost ? createPortal(modal, portalHost) : modal;
}

function HistoryAllRow({
  item,
  index,
  kind,
  onOpenTarget
}: {
  item: JsonRecord;
  index: number;
  kind: HistoryRankKind | 'recent';
  onOpenTarget: (target: HistoryTarget | null) => void;
}) {
  const source = (item.source || {}) as JsonRecord;
  const isRecent = kind === 'recent';
  const isArtist = kind === 'artist';
  const isSong = kind === 'song';
  const name = isRecent
    ? String(item.title || source.title || 'Unknown track')
    : String(item.name || item.title || item.album || 'Unknown');
  const playLabel = `${Number(item.play_count || 0)} plays`;
  const subtitle = isRecent
    ? String(item.artist || source.artist || 'Unknown artist')
    : isSong && item.subtitle
      ? `${String(item.subtitle)} - ${playLabel}`
      : String(item.subtitle || item.artist || item.album_artist || playLabel);
  const listenedSeconds = isRecent ? item.played_secs : item.listened_secs;
  const target = isRecent ? historyRecentTrackTarget(item) : historyRankTarget(item, kind);

  return (
    <button
      className="history-all-row history-click-row"
      type="button"
      onClick={() => onOpenTarget(target)}
    >
      <span className="history-rank-index">{index + 1}</span>
      <div className={`history-rank-art${isArtist ? ' history-artist-avatar' : ''}`}>
        {isArtist ? (
          <ArtistAvatar name={name} />
        ) : (
          <HistoryArtwork item={isRecent ? { ...source, ...item } : item} />
        )}
      </div>
      <div className="history-rank-text">
        <strong title={name}>{name}</strong>
        <span>{subtitle}</span>
      </div>
      <em>{formatListeningTime(listenedSeconds)}</em>
    </button>
  );
}

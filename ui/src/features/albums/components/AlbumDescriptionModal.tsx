import { Icon } from '../../../shared/ui/Icon';
import { Modal } from '../../../shared/ui/Modal';

export function AlbumDescriptionModal({
  title,
  artist,
  year,
  label = 'About this album',
  paragraphs,
  onClose
}: {
  title: string;
  artist?: string;
  year?: unknown;
  label?: string;
  paragraphs: string[];
  onClose: () => void;
}) {
  const subtitle = [artist, year ? String(year) : ''].filter(Boolean).join(' / ');
  return (
    <Modal
      open
      className="album-description-backdrop"
      ariaLabelledBy="album-description-title"
      onClose={onClose}
    >
      <div className="album-description-panel">
        <header className="album-description-head">
          <div>
            <div className="section-label">{label}</div>
            <h2 id="album-description-title">{title}</h2>
            {subtitle ? <p>{subtitle}</p> : null}
          </div>
          <button
            className="album-description-close"
            type="button"
            aria-label="Close description"
            onClick={onClose}
          >
            <Icon path="M18 6 6 18M6 6l12 12" />
          </button>
        </header>
        <div className="album-description-body">
          {paragraphs.map((paragraph, index) => (
            <p key={`${index}-${paragraph.slice(0, 18)}`}>{paragraph}</p>
          ))}
        </div>
      </div>
    </Modal>
  );
}

import {
  collectAlbumCredits,
  creditGroups,
  positiveNumber,
  safeArray,
  stringArray,
  titleOf,
  trackCreditRows
} from '../../../shared/lib/appSupport';
import { stripFileExtension } from '../../../shared/lib/format';
import type { JsonRecord, LibraryTrack } from '../../../shared/types';

export function AlbumCreditsPanel({
  title,
  artist,
  tracks,
  infoItems
}: {
  title: string;
  artist: string;
  tracks: LibraryTrack[];
  infoItems: Array<[string, string]>;
}) {
  const grouped = collectAlbumCredits(tracks);
  const groupedCreditCount = Object.values(grouped).reduce(
    (groupSum, roles) =>
      groupSum + Object.values(roles).reduce((roleSum, names) => roleSum + names.size, 0),
    0
  );
  const allGroups: Array<{
    id: string;
    title: string;
    roles: Record<string, Map<string, string>>;
  }> = [
    ...creditGroups.map((group) => ({
      id: group.id,
      title: group.title,
      roles: grouped[group.id] || {}
    })),
    { id: 'other', title: 'Other Credits', roles: grouped.other || {} }
  ].filter((group) => Object.keys(group.roles).length > 0);
  const tracksByDisc = tracks.reduce<Record<string, LibraryTrack[]>>((groups, track) => {
    const disc = String(positiveNumber(track.disc_number) || 1);
    groups[disc] = groups[disc] || [];
    groups[disc].push(track);
    return groups;
  }, {});
  const discNumbers = Object.keys(tracksByDisc)
    .map(Number)
    .sort((a, b) => a - b);
  const multiDisc = discNumbers.length > 1;

  return (
    <div className="credits-layout">
      <section className="panel padded-lg credits-album-panel">
        <header className="credits-album-head">
          <div>
            <span className="label">Credits</span>
            <h2 className="credits-album-title">{title}</h2>
            {artist ? <p>{artist}</p> : null}
          </div>
        </header>
        <div className="credits-info-grid">
          {infoItems.map(([label, value]) => (
            <div className="credits-info-item" key={label}>
              <span className="label">{label}</span>
              <strong>{value}</strong>
            </div>
          ))}
        </div>
      </section>

      <section className="panel padded-lg credits-roles-panel">
        <header className="credits-tracks-head">
          <div>
            <span className="label">Contributor credits</span>
            {groupedCreditCount ? (
              <strong>
                {groupedCreditCount} listed credit{groupedCreditCount === 1 ? '' : 's'}
              </strong>
            ) : null}
          </div>
          {!groupedCreditCount ? (
            <span className="album-subtitle">
              No grouped contributor credits in the current metadata.
            </span>
          ) : null}
        </header>
        <div className="credits-role-grid">
          {allGroups.map((group) => (
            <section className="credits-role-group" key={group.id}>
              <header className="credits-role-group-head">
                <h3>{group.title}</h3>
                <span>
                  {Object.values(group.roles).reduce((sum, names) => sum + names.size, 0)} names
                </span>
              </header>
              <div className="credits-role-list">
                {Object.entries(group.roles).map(([role, names]) => (
                  <div className="credits-role-row" key={role}>
                    <span className="label">{role}</span>
                    <div>
                      {Array.from(names.values())
                        .sort((a, b) => a.localeCompare(b))
                        .map((name) => (
                          <span className="credit-name" key={name}>
                            {name}
                          </span>
                        ))}
                    </div>
                  </div>
                ))}
              </div>
            </section>
          ))}
        </div>
      </section>

      <section className="panel padded-lg credits-tracks-panel">
        <header className="credits-tracks-head">
          <div>
            <span className="label">Track-level credits</span>
            <strong>
              {tracks.length} track{tracks.length === 1 ? '' : 's'}
            </strong>
          </div>
        </header>
        {discNumbers.map((disc) => (
          <div className="react-credits-disc-section" key={disc}>
            {multiDisc ? (
              <div className="credits-disc-head">
                <span className="label">Disc {disc}</span>
              </div>
            ) : null}
            <div className="credits-track-detail-list">
              {(tracksByDisc[String(disc)] || []).map((track, index) => {
                const rows = trackCreditRows(track);
                const creditCount = safeArray<JsonRecord>(track.credits).reduce(
                  (sum, credit) => sum + stringArray(credit.roles).length,
                  0
                );
                const sub = [
                  track.artist,
                  track.composer ? `Composer: ${track.composer}` : '',
                  creditCount ? `${creditCount} credit${creditCount === 1 ? '' : 's'}` : ''
                ]
                  .filter(Boolean)
                  .join(' / ');
                return (
                  <details
                    className="credits-track-detail"
                    key={String(track.id || track.track_id || track.file_name || index)}
                  >
                    <summary>
                      <span className="credits-num">
                        {positiveNumber(track.track_number) || index + 1}
                      </span>
                      <span>
                        <strong>{titleOf(track, stripFileExtension(track.file_name))}</strong>
                        {sub ? <small>{sub}</small> : null}
                      </span>
                    </summary>
                    {rows.length ? (
                      <dl className="credits-track-roles">
                        {rows.map(([role, value]) => (
                          <div key={`${role}-${value}`}>
                            <dt>{role}</dt>
                            <dd>{value}</dd>
                          </div>
                        ))}
                      </dl>
                    ) : (
                      <div className="album-subtitle">No detailed credits for this track.</div>
                    )}
                  </details>
                );
              })}
            </div>
          </div>
        ))}
      </section>
    </div>
  );
}

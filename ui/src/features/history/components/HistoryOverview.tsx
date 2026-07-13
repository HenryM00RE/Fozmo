import { safeArray } from '../../../shared/lib/appSupport';
import type { JsonRecord } from '../../../shared/types';
import {
  formatHistoryBucketLabel,
  formatListeningTime,
  historyBucketKey
} from '../model/historyModel';

export function HistoryOverview({ stats }: { stats: JsonRecord | null }) {
  const weeklyBuckets = safeArray<JsonRecord>(stats?.weekly_buckets);
  const weekdayBuckets = safeArray<JsonRecord>(stats?.weekday_buckets);
  const maxWeek = Math.max(1, ...weeklyBuckets.map((bucket) => Number(bucket.listened_secs || 0)));
  const maxDay = Math.max(1, ...weekdayBuckets.map((bucket) => Number(bucket.listened_secs || 0)));

  return (
    <section className="history-overview">
      <div className="history-total">
        <span className="history-total-label">Time listened</span>
        <strong>{formatListeningTime(stats?.total_listened_secs)}</strong>
      </div>

      <div className="history-chart">
        <span className="history-chart-label">Listening over time</span>
        <div className="history-weeks">
          {weeklyBuckets.map((bucket, index) => {
            const listened = Number(bucket.listened_secs || 0);
            const height = Math.max(2, Math.round((listened / maxWeek) * 100));
            return (
              <div className="history-week-column" key={historyBucketKey(bucket, index)}>
                <div
                  className={`history-week-bar${listened ? '' : ' is-empty'}`}
                  title={formatListeningTime(listened)}
                >
                  <i style={{ height: `${height}%` }} />
                </div>
                <span>{formatHistoryBucketLabel(bucket)}</span>
                <em>{formatListeningTime(listened)}</em>
              </div>
            );
          })}
        </div>
      </div>

      <div className="history-chart">
        <span className="history-chart-label">By day of week</span>
        <div className="history-weekdays">
          {weekdayBuckets.map((bucket, index) => {
            const listened = Number(bucket.listened_secs || 0);
            const height = Math.max(2, Math.round((listened / maxDay) * 100));
            return (
              <div className="history-day-column" key={historyBucketKey(bucket, index)}>
                <div
                  className={`history-day-bar${listened ? '' : ' is-empty'}`}
                  title={formatListeningTime(listened)}
                >
                  <i style={{ height: `${height}%` }} />
                </div>
                <span>{String(bucket.label || '')}</span>
                <em>{formatListeningTime(listened)}</em>
              </div>
            );
          })}
        </div>
      </div>
    </section>
  );
}

import {
  isAirPlayZone,
  isKefZone,
  isLocalOutputZone,
  isSonosZone
} from '../../features/settings/settingsModel';
import { isBrowserZone } from '../lib/browserZone';
import type { ZoneProfile } from '../types';
import { ChromeIcon, IPhoneIcon } from './BrowserDeviceIcon';
import { HegelIcon } from './HegelIcon';
import { Icon } from './Icon';
import { KefIcon } from './KefIcon';
import { MacMiniIcon } from './MacMiniIcon';
import { SonosIcon } from './SonosIcon';
import { SpeakerIcon } from './SpeakerIcon';

export type OutputIconId =
  | 'auto'
  | 'hegel'
  | 'mac_mini'
  | 'sonos'
  | 'kef'
  | 'airplay'
  | 'computer'
  | 'speaker';

export const outputIconOptions: { value: OutputIconId; label: string }[] = [
  { value: 'auto', label: 'Auto' },
  { value: 'hegel', label: 'Hegel' },
  { value: 'mac_mini', label: 'Mac mini' },
  { value: 'sonos', label: 'Sonos' },
  { value: 'kef', label: 'KEF' },
  { value: 'airplay', label: 'AirPlay' },
  { value: 'computer', label: 'Computer' },
  { value: 'speaker', label: 'Speaker' }
];

const outputIconIds = new Set<OutputIconId>(outputIconOptions.map((option) => option.value));

export function isOutputIconId(value: unknown): value is OutputIconId {
  return outputIconIds.has(value as OutputIconId);
}

export function savedOutputIcon(zone: ZoneProfile): OutputIconId {
  const icon = String(zone.icon || '').trim();
  return isOutputIconId(icon) ? icon : 'auto';
}

export function resolvedOutputIcon(
  zone?: ZoneProfile,
  hegelZoneId = ''
): Exclude<OutputIconId, 'auto'> | 'browser_chrome' | 'browser_ios' | 'browser_other' {
  if (zone) {
    const saved = savedOutputIcon(zone);
    if (saved !== 'auto') return saved;
    if (zone.device_type === 'hegel') return 'hegel';
    if (hegelZoneId && zone.id === hegelZoneId) return 'hegel';
    if (isSonosZone(zone)) return 'sonos';
    if (isKefZone(zone)) return 'kef';
    if (isAirPlayZone(zone)) return 'airplay';
    if (isBrowserZone(zone)) {
      if (/(?:iphone|ipad|ios)/i.test(zone.name)) return 'browser_ios';
      return /^chrome(?:\s+on\s+|$)/i.test(zone.name) ? 'browser_chrome' : 'browser_other';
    }
    if (isLocalOutputZone(zone)) return 'mac_mini';
    if (zone.protocol === 'remote_agent') return 'computer';
  }
  return 'speaker';
}

export function ZoneOutputIcon({
  detail = 'simple',
  hegelZoneId,
  icon = 'auto',
  zone
}: {
  detail?: 'simple' | 'panel';
  hegelZoneId?: string;
  icon?: OutputIconId;
  zone?: ZoneProfile;
}) {
  const resolved = icon && icon !== 'auto' ? icon : resolvedOutputIcon(zone, hegelZoneId);

  switch (resolved) {
    case 'browser_ios':
      return <IPhoneIcon className="iphone-zone-icon" />;
    case 'browser_chrome':
      return <ChromeIcon className="chrome-zone-icon" />;
    case 'browser_other':
      return (
        <Icon path="M12 22a10 10 0 1 0 0-20 10 10 0 0 0 0 20ZM2 12h20M12 2a15.3 15.3 0 0 1 4 10 15.3 15.3 0 0 1-4 10 15.3 15.3 0 0 1-4-10 15.3 15.3 0 0 1 4-10Z" />
      );
    case 'hegel':
      return <HegelIcon className="hegel-zone-icon" detail={detail} />;
    case 'mac_mini':
      return <MacMiniIcon className="macmini-zone-icon" detail={detail} />;
    case 'sonos':
      return <SonosIcon className="sonos-zone-icon" />;
    case 'kef':
      return <KefIcon className="kef-zone-icon" />;
    case 'airplay':
      return (
        <Icon path="M5 17H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h16a2 2 0 0 1 2 2v10a2 2 0 0 1-2 2h-1M12 11l5 6H7z" />
      );
    case 'computer':
      return <Icon path="M4 5h16v10H4zM9 21h6M12 15v6" />;
    case 'speaker':
      return <SpeakerIcon className="speaker-zone-icon" />;
  }
}

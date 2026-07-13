import type { ZoneProfile } from '../../../shared/types';
import { SelectMenu } from '../../../shared/ui/SelectMenu';
import { zoneDisplayName } from '../settingsModel';

export function SettingsZoneSelect({
  ariaLabel,
  onChange,
  selectedZoneLabel,
  selectedZoneId,
  zones
}: {
  ariaLabel: string;
  onChange: (zoneId: string) => void;
  selectedZoneLabel: string;
  selectedZoneId: string;
  zones: ZoneProfile[];
}) {
  const options = zones.map((zone) => ({
    value: zone.id,
    label:
      zone.id === selectedZoneId && selectedZoneLabel ? selectedZoneLabel : zoneDisplayName(zone)
  }));

  return (
    <SelectMenu
      ariaLabel={ariaLabel}
      className="dsp-selected-device settings-zone-select"
      menuMinWidth={260}
      value={selectedZoneId || options[0]?.value || ''}
      onChange={onChange}
      options={options}
    />
  );
}

import type { ZoneProfile } from '../../../shared/types';
import { type HegelModelId, hegelHostFromZone, hegelModels } from '../settingsModel';

export type HegelAirplayCandidate = {
  zone: ZoneProfile;
  host: string;
};

export type HegelControlActions = {
  refreshStatus: () => Promise<void>;
};

export function hegelInputOptions(modelId: HegelModelId) {
  return Array.from({ length: hegelModels[modelId].inputs }, (_, index) => index + 1);
}

export function hegelAirplayCandidates(zones: ZoneProfile[]): HegelAirplayCandidate[] {
  return zones
    .map((zone) => ({
      zone,
      host: hegelHostFromZone(zone),
      backend: String(zone.backend || zone.protocol || '').toLowerCase()
    }))
    .filter((candidate) => candidate.host && candidate.backend.includes('airplay'))
    .map(({ zone, host }) => ({ zone, host }));
}

export function hegelInputLabel(input: number, modelId: HegelModelId) {
  const model = hegelModels[modelId];
  if ('xlr' in model && input === model.xlr) return `XLR / Balanced (input ${input})`;
  return input === model.usb ? `USB (input ${input})` : `Input ${input}`;
}

export function hegelSavedInputLabel(input: number, modelId: HegelModelId) {
  return hegelInputLabel(input, modelId).replace(/\s+\(input \d+\)$/, '');
}

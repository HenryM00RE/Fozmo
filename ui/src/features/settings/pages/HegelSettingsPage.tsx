import type { Dispatch, SetStateAction } from 'react';
import type { ZoneProfile } from '../../../shared/types';
import { HegelSettingsForm } from '../components/HegelSettingsForm';
import type { HegelControlActions } from '../model/hegelFormModel';
import type { HegelFormState } from '../settingsModel';

export function HegelSettingsPage({
  hegelControls,
  hegelMessage,
  hegelSettings,
  setHegelSettings,
  zones
}: {
  hegelControls: HegelControlActions;
  hegelMessage: string;
  hegelSettings: HegelFormState;
  setHegelSettings: Dispatch<SetStateAction<HegelFormState>>;
  zones: ZoneProfile[];
}) {
  return (
    <section className="settings-panel">
      <HegelSettingsForm
        hegelControls={hegelControls}
        hegelMessage={hegelMessage}
        hegelSettings={hegelSettings}
        setHegelSettings={setHegelSettings}
        zones={zones}
      />
    </section>
  );
}

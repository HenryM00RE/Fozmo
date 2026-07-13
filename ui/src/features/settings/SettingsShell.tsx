import type { ReactNode } from 'react';

export function SettingsShell({ children }: { children: ReactNode }) {
  return (
    <section className="view settings-view">
      <div className="settings-layout">
        <div className="settings-content">{children}</div>
      </div>
    </section>
  );
}

// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { GeneralSettingsPage } from './GeneralSettingsPage';

afterEach(cleanup);

describe('GeneralSettingsPage history export', () => {
  it('opens a range dialog and exports the selected time span', async () => {
    const exportHistory = vi.fn(async () => true);
    render(
      <div className="react-app">
        <GeneralSettingsPage
          addFolder={async () => undefined}
          clearQobuzCache={async () => undefined}
          dataStatus="Ready"
          exportHistory={exportHistory}
          folderInput=""
          folderStatus=""
          folders={[]}
          importFile={null}
          importHistory={async () => undefined}
          importMode="merge"
          isPickingFolder={false}
          isScanning={false}
          libraryManagementAvailable={false}
          pickFolder={async () => undefined}
          removeFolder={async () => undefined}
          removingFolder=""
          qobuzCache={null}
          rescan={async () => undefined}
          scanProgress={null}
          scanStatus=""
          setImportFile={() => undefined}
          setImportMode={() => undefined}
          setTheme={() => undefined}
          setFolderInput={() => undefined}
          theme="dark"
        />
      </div>
    );

    fireEvent.click(screen.getByRole('button', { name: 'Export history' }));
    expect(screen.getByRole('dialog', { name: 'Export history' })).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Export time span' })).toHaveTextContent('All time');

    fireEvent.click(screen.getByRole('button', { name: 'Export time span' }));
    fireEvent.click(screen.getByRole('option', { name: 'Last week' }));
    fireEvent.click(screen.getByRole('button', { name: 'Export' }));

    await waitFor(() => expect(exportHistory).toHaveBeenCalledWith('week'));
    await waitFor(() =>
      expect(screen.queryByRole('dialog', { name: 'Export history' })).not.toBeInTheDocument()
    );
  });
});

import { type ChangeEvent, useState } from 'react';
import { type WalnutTheme, walnutThemes } from '../../../shared/lib/theme';
import type { JsonRecord } from '../../../shared/types';
import { Icon } from '../../../shared/ui/Icon';
import { Modal } from '../../../shared/ui/Modal';
import { SelectMenu } from '../../../shared/ui/SelectMenu';
import type { ScanProgress } from '../hooks/useMediaSettings';
import { qobuzCacheSummary } from '../model/qobuzSettingsModel';

const themeLabels: Record<WalnutTheme, string> = {
  light: 'Walnut',
  neutral: 'Neutral',
  dark: 'Dark'
};

const themeOptions = walnutThemes.map((choice) => ({
  value: choice,
  label: themeLabels[choice]
}));

const importModeOptions = [
  { value: 'merge', label: 'Merge' },
  { value: 'replace', label: 'Replace' }
];

export function GeneralSettingsPage({
  addFolder,
  clearQobuzCache,
  dataStatus,
  exportHistory,
  folderInput,
  folderStatus,
  folders,
  importFile,
  importHistory,
  importMode,
  isPickingFolder,
  isScanning,
  libraryManagementAvailable,
  pickFolder,
  removeFolder,
  removingFolder,
  qobuzCache,
  rescan,
  scanProgress,
  scanStatus,
  setImportFile,
  setImportMode,
  setTheme,
  theme,
  setFolderInput
}: {
  addFolder: () => Promise<void>;
  clearQobuzCache: () => Promise<void>;
  dataStatus: string;
  exportHistory: () => Promise<void>;
  folderInput: string;
  folderStatus: string;
  folders: string[];
  importFile: File | null;
  importHistory: () => Promise<void>;
  importMode: string;
  isPickingFolder: boolean;
  isScanning: boolean;
  libraryManagementAvailable: boolean;
  pickFolder: () => Promise<void>;
  removeFolder: (path: string) => Promise<void>;
  removingFolder: string;
  qobuzCache: JsonRecord | null;
  rescan: () => Promise<void>;
  scanProgress: ScanProgress | null;
  scanStatus: string;
  setImportFile: (file: File | null) => void;
  setImportMode: (mode: string) => void;
  setTheme: (theme: WalnutTheme) => void;
  setFolderInput: (value: string) => void;
  theme: WalnutTheme;
}) {
  const [importOpen, setImportOpen] = useState(false);
  const [folderToRemove, setFolderToRemove] = useState<string | null>(null);
  const [folderRemoveError, setFolderRemoveError] = useState('');
  const chooseTheme = (value: string) => {
    if (walnutThemes.includes(value as WalnutTheme)) setTheme(value as WalnutTheme);
  };
  const runImportHistory = async () => {
    await importHistory();
    setImportOpen(false);
  };
  const closeFolderRemove = () => {
    if (removingFolder) return;
    setFolderToRemove(null);
    setFolderRemoveError('');
  };
  const confirmFolderRemove = async () => {
    if (!folderToRemove || removingFolder) return;
    setFolderRemoveError('');
    try {
      await removeFolder(folderToRemove);
      setFolderToRemove(null);
    } catch (error) {
      setFolderRemoveError(error instanceof Error ? error.message : 'Could not remove folder');
    }
  };

  return (
    <>
      <section className="settings-panel general-settings-panel">
        <section className="settings-section-block">
          <div className="settings-section-heading">
            <div className="section-label">General</div>
          </div>
          <div className="panel raised">
            <div className="settings-list">
              {libraryManagementAvailable ? (
                <>
                  <div className="library-folder-row">
                    <label className="field">
                      <span>Music folder</span>
                      <input
                        type="text"
                        value={folderInput}
                        onChange={(event) => setFolderInput(event.target.value)}
                        placeholder="Paste a music folder path"
                      />
                    </label>
                    <button className="pill primary" type="button" onClick={addFolder}>
                      <Icon path="M12 5v14M5 12h14" />
                      Add folder
                    </button>
                    <button
                      className="pill"
                      type="button"
                      onClick={pickFolder}
                      disabled={isPickingFolder}
                    >
                      <Icon path="M3 7h6l2 2h10v9a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2Z" />
                      {isPickingFolder ? 'Choosing...' : 'Choose folder'}
                    </button>
                  </div>
                  <div className="library-folder-list" aria-label="Current folders">
                    <div className="library-folder-list-label">Current folders</div>
                    {folders.length ? (
                      folders.map((folder) => (
                        <LibraryFolderItem
                          folder={folder}
                          key={folder}
                          removing={removingFolder === folder}
                          onRemove={() => {
                            setFolderRemoveError('');
                            setFolderToRemove(folder);
                          }}
                        />
                      ))
                    ) : (
                      <div className="library-folder-empty">
                        {folderStatus || 'No folders added yet.'}
                      </div>
                    )}
                  </div>
                  <div className="setting-row rescan-music-row">
                    <span className="scan-progress-copy">
                      <strong>Rescan music</strong>
                      <small>{scanStatusLabel(scanProgress, scanStatus)}</small>
                      <ScanProgressBar progress={scanProgress} />
                    </span>
                    <button
                      className="settings-heading-refresh"
                      type="button"
                      title="Rescan music"
                      aria-label={isScanning ? 'Scanning music' : 'Rescan music'}
                      onClick={rescan}
                      disabled={isScanning}
                    >
                      {isScanning ? (
                        <span className="settings-refresh-spinner" aria-hidden="true" />
                      ) : (
                        <Icon path="M21 3v5h-5M20.1 13.5a7.5 7.5 0 1 1-2-7.1L21 8" />
                      )}
                    </button>
                  </div>
                </>
              ) : (
                <div className="setting-row general-library-host-notice">
                  <span>
                    <strong>Library management</strong>
                    <small>
                      Music folders and library rescans must be managed on the server device itself.
                    </small>
                  </span>
                </div>
              )}
              <div className="setting-row control-row general-theme-row">
                <span>
                  <strong>Theme</strong>
                  <small>Choose the interface theme.</small>
                </span>
                <SelectMenu
                  className="theme-select"
                  ariaLabel="Theme"
                  value={theme}
                  onChange={chooseTheme}
                  options={themeOptions}
                />
              </div>
              <div className="setting-row">
                <span>
                  <strong>Export history</strong>
                  <small>{dataStatus}</small>
                </span>
                <button
                  className="settings-row-icon-button"
                  type="button"
                  title="Export history"
                  aria-label="Export history"
                  onClick={exportHistory}
                >
                  <Icon path="M12 3v12M7 8l5-5 5 5M5 21h14" />
                </button>
              </div>
              <div className="setting-row">
                <span>
                  <strong>Import history</strong>
                  <small>{importFile?.name || 'Choose a JSON export.'}</small>
                </span>
                <button
                  className="settings-row-icon-button"
                  type="button"
                  title="Import history"
                  aria-label="Import history"
                  onClick={() => setImportOpen(true)}
                >
                  <Icon path="M12 3v12M7 10l5 5 5-5M5 21h14" />
                </button>
              </div>
              <div className="setting-row">
                <span>
                  <strong>Clear Qobuz cache</strong>
                  <small>{qobuzCacheSummary(qobuzCache)}</small>
                </span>
                <button
                  className="settings-row-icon-button danger"
                  type="button"
                  title="Clear Qobuz cache"
                  aria-label="Clear Qobuz cache"
                  onClick={clearQobuzCache}
                >
                  <Icon path="M3 6h18M8 6V4h8v2M6 6l1 15h10l1-15M10 11v6M14 11v6" />
                </button>
              </div>
            </div>
          </div>
        </section>
      </section>
      <Modal
        className="folder-remove-backdrop"
        ariaLabelledBy="folder-remove-title"
        open={Boolean(folderToRemove)}
        onClose={closeFolderRemove}
      >
        <section className="folder-remove-panel" onMouseDown={(event) => event.stopPropagation()}>
          <div className="folder-remove-body">
            <div className="section-label">Music folder</div>
            <h2 id="folder-remove-title">Remove this folder?</h2>
            <p className="folder-remove-path" title={folderToRemove || ''}>
              {folderToRemove ? compactMusicFolderPath(folderToRemove) : ''}
            </p>
            <p>
              This won’t delete or change any music or other content in the folder. It only removes
              the location from Fozmo, so the app will stop checking it for music.
            </p>
            {folderRemoveError ? (
              <p className="folder-remove-error" role="alert">
                {folderRemoveError}
              </p>
            ) : null}
          </div>
          <footer className="folder-remove-foot">
            <button
              className="pill"
              type="button"
              onClick={closeFolderRemove}
              disabled={Boolean(removingFolder)}
            >
              Cancel
            </button>
            <button
              className="pill danger"
              type="button"
              onClick={() => void confirmFolderRemove()}
              disabled={Boolean(removingFolder)}
            >
              {removingFolder ? 'Removing...' : 'Remove folder'}
            </button>
          </footer>
        </section>
      </Modal>
      <Modal
        className="history-import-backdrop"
        ariaLabelledBy="history-import-title"
        open={importOpen}
        onClose={() => setImportOpen(false)}
      >
        <div className="history-import-panel">
          <div className="history-import-head">
            <div>
              <div className="section-label">History</div>
              <h2 id="history-import-title">Import history</h2>
            </div>
            <button
              className="history-import-close"
              type="button"
              aria-label="Close import history"
              onClick={() => setImportOpen(false)}
            >
              <Icon path="M18 6 6 18M6 6l12 12" />
            </button>
          </div>
          <div className="history-import-body">
            <div className="history-import-row control-row">
              <span>
                <strong>Import mode</strong>
                <small>
                  {importMode === 'replace'
                    ? 'Replace existing history.'
                    : 'Add entries to existing history.'}
                </small>
              </span>
              <SelectMenu
                ariaLabel="Import mode"
                value={importMode}
                onChange={setImportMode}
                options={importModeOptions}
              />
            </div>
            <div className="history-import-row">
              <span>
                <strong>Import file</strong>
                <small>{importFile?.name || 'No file selected'}</small>
              </span>
              <label className="pill import-file-picker">
                <Icon path="M12 3v12M7 8l5-5 5 5M5 21h14" />
                Choose file
                <input
                  className="sr-only"
                  type="file"
                  accept="application/json,.json"
                  onChange={(event: ChangeEvent<HTMLInputElement>) =>
                    setImportFile(event.target.files?.[0] || null)
                  }
                />
              </label>
            </div>
          </div>
          <div className="history-import-foot">
            <button className="pill" type="button" onClick={() => setImportOpen(false)}>
              Cancel
            </button>
            <button
              className="pill primary"
              type="button"
              disabled={!importFile}
              onClick={runImportHistory}
            >
              Import
            </button>
          </div>
        </div>
      </Modal>
    </>
  );
}

function LibraryFolderItem({
  folder,
  onRemove,
  removing
}: {
  folder: string;
  onRemove: () => void;
  removing: boolean;
}) {
  const label = compactMusicFolderPath(folder);

  return (
    <div className="library-folder-item" title={folder}>
      <Icon path="M3 7.5A2.5 2.5 0 0 1 5.5 5H9l2 2h7.5A2.5 2.5 0 0 1 21 9.5v7A2.5 2.5 0 0 1 18.5 19h-13A2.5 2.5 0 0 1 3 16.5v-9Z" />
      <span>{label}</span>
      <button
        className="library-folder-remove"
        type="button"
        aria-label={`Remove music folder ${label}`}
        onClick={onRemove}
        disabled={removing}
      >
        <Icon path="M5 12h14" />
      </button>
    </div>
  );
}

function compactMusicFolderPath(folder: string) {
  const normalized = folder.replace(/\\/g, '/').replace(/\/+$/, '');
  const parts = normalized.split('/').filter(Boolean);
  if (parts.length >= 3 && parts[0] === 'Users') return `~/${parts.slice(2).join('/')}`;
  return parts.length > 3 ? `.../${parts.slice(-3).join('/')}` : normalized || folder;
}

function formatScanNumber(value: number) {
  return Math.max(0, Math.round(value)).toLocaleString();
}

function scanCountLabel(progress: ScanProgress) {
  if (progress.total > 0) {
    return `Scanned ${formatScanNumber(progress.scanned)} of ${formatScanNumber(progress.total)} files`;
  }
  if (progress.running && progress.phase === 'preparing') {
    if (progress.scanned > 0) return `Found ${formatScanNumber(progress.scanned)} audio files`;
    return progress.message || 'Finding audio files...';
  }
  return progress.message || 'Ready';
}

function scanStatusLabel(progress: ScanProgress | null, fallback: string) {
  if (!progress || progress.phase === 'idle') return fallback;
  if (progress.error) return progress.error;
  if (progress.phase === 'complete') {
    return `Indexed ${formatScanNumber(progress.scanned)} files • ${formatScanNumber(progress.updated)} updated • ${formatScanNumber(progress.removed)} removed`;
  }
  if (progress.phase === 'cleanup') {
    return `Finalizing library index • ${scanCountLabel(progress)}`;
  }
  if (progress.running) return scanCountLabel(progress);
  return progress.message || fallback;
}

function ScanProgressBar({ progress }: { progress: ScanProgress | null }) {
  if (!progress || progress.phase === 'idle') return null;
  const hasTotal = progress.total > 0;
  const percent = hasTotal
    ? Math.max(0, Math.min(100, (progress.scanned / progress.total) * 100))
    : 0;
  const detail = [
    progress.currentPath ? `Now: ${progress.currentPath}` : null,
    progress.updated ? `${formatScanNumber(progress.updated)} updated` : null,
    progress.removed ? `${formatScanNumber(progress.removed)} removed` : null,
    progress.running && !hasTotal ? 'Preparing file list' : null
  ]
    .filter(Boolean)
    .join(' • ');

  return (
    <div
      className="scan-progress"
      data-running={progress.running ? 'true' : 'false'}
      data-indeterminate={progress.running && !hasTotal ? 'true' : 'false'}
    >
      <div className="scan-progress-track" aria-hidden="true">
        <span style={{ width: hasTotal ? `${percent}%` : '35%' }} />
      </div>
      {detail ? <small className="scan-progress-meta">{detail}</small> : null}
    </div>
  );
}

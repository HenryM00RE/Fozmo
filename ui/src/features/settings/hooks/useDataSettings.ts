import { useState } from 'react';
import { APP_SLUG } from '../../../shared/identity';
import { api } from '../../../shared/lib/api';
import { safeArray } from '../../../shared/lib/appSupport';
import type { JsonRecord } from '../../../shared/types';
import { numberValue } from '../settingsModel';

export function useDataSettings() {
  const [importMode, setImportMode] = useState('merge');
  const [importFile, setImportFile] = useState<File | null>(null);
  const [dataStatus, setDataStatus] = useState('Ready');

  const exportHistory = async () => {
    setDataStatus('Preparing export...');
    try {
      const data = await api.get<JsonRecord>('/api/history/export');
      const blob = new Blob([JSON.stringify(data, null, 2)], { type: 'application/json' });
      const url = URL.createObjectURL(blob);
      const link = document.createElement('a');
      link.href = url;
      link.download = `${APP_SLUG}-listening-history-${new Date().toISOString().slice(0, 10)}.json`;
      document.body.appendChild(link);
      link.click();
      link.remove();
      URL.revokeObjectURL(url);
      setDataStatus(`${safeArray(data.entries).length} entries exported`);
    } catch {
      setDataStatus('Export failed');
    }
  };

  const importHistory = async () => {
    if (!importFile) return;
    setDataStatus('Importing...');
    try {
      const parsed = JSON.parse(await importFile.text()) as JsonRecord | JsonRecord[];
      const entries = Array.isArray(parsed) ? parsed : safeArray(parsed.entries);
      const result = await api.post<JsonRecord>('/api/history/import', {
        mode: importMode,
        entries
      });
      setDataStatus(
        `${numberValue(result.imported, 0)} imported, ${numberValue(result.skipped, 0)} skipped`
      );
    } catch {
      setDataStatus('Import failed');
    }
  };

  return {
    dataStatus,
    exportHistory,
    importFile,
    importHistory,
    importMode,
    setImportFile,
    setImportMode
  };
}

import { useCallback, useEffect, useRef, useState } from 'react';
import { endpoints } from '../../../shared/lib/api';
import type { JsonRecord } from '../../../shared/types';
import { numberValue } from '../settingsModel';

export type ScanProgress = {
  running: boolean;
  phase: string;
  scanned: number;
  total: number;
  updated: number;
  removed: number;
  currentPath: string;
  message: string;
  error: string;
};

export function useMediaSettings(onRefresh: () => Promise<void>, enabled = true) {
  const [folders, setFolders] = useState<string[]>([]);
  const [folderInput, setFolderInput] = useState('');
  const [folderStatus, setFolderStatus] = useState('Loading folders...');
  const [isPickingFolder, setIsPickingFolder] = useState(false);
  const [removingFolder, setRemovingFolder] = useState('');
  const [scanStatus, setScanStatus] = useState('Ready');
  const [scanProgress, setScanProgress] = useState<ScanProgress | null>(null);
  const [isScanning, setIsScanning] = useState(false);
  const scanPollRef = useRef<number | null>(null);

  const reloadFolders = useCallback(() => {
    return endpoints
      .folders()
      .then((response) => {
        const nextFolders = response.folders || [];
        setFolders(nextFolders);
        setFolderStatus(nextFolders.length ? '' : 'No folders added yet.');
      })
      .catch((error) => {
        setFolders([]);
        setFolderStatus(error instanceof Error ? error.message : 'Could not load folders');
      });
  }, []);

  useEffect(() => {
    if (!enabled) return;
    reloadFolders();
  }, [enabled, reloadFolders]);

  const stopScanPolling = useCallback(() => {
    if (scanPollRef.current !== null) {
      window.clearInterval(scanPollRef.current);
      scanPollRef.current = null;
    }
  }, []);

  useEffect(() => stopScanPolling, [stopScanPolling]);

  const applyScanProgress = useCallback((payload: JsonRecord) => {
    const progress: ScanProgress = {
      running: Boolean(payload.running),
      phase: String(payload.phase || 'idle'),
      scanned: numberValue(payload.scanned, 0),
      total: numberValue(payload.total, 0),
      updated: numberValue(payload.updated, 0),
      removed: numberValue(payload.removed, 0),
      currentPath: String(payload.current_path || ''),
      message: String(payload.message || ''),
      error: String(payload.error || '')
    };
    setScanProgress(progress);
    if (progress.error) {
      setScanStatus(progress.error);
    } else if (progress.message) {
      setScanStatus(
        progress.currentPath ? `${progress.message} • ${progress.currentPath}` : progress.message
      );
    }
    return progress;
  }, []);

  const pollScanProgress = useCallback(async () => {
    try {
      return applyScanProgress(await endpoints.rescanStatus());
    } catch {
      return null;
    }
  }, [applyScanProgress]);

  useEffect(() => {
    if (!enabled) return undefined;
    let cancelled = false;
    const pollRunningScan = async () => {
      const progress = await pollScanProgress();
      if (cancelled || !progress) return;
      setIsScanning(progress.running);
      if (!progress.running) {
        stopScanPolling();
      }
    };

    pollRunningScan();
    scanPollRef.current = window.setInterval(() => {
      void pollRunningScan();
    }, 450);

    return () => {
      cancelled = true;
    };
  }, [enabled, pollScanProgress, stopScanPolling]);

  const addFolderPath = async (path: string, clearInput = false) => {
    if (!path) return;
    setFolderStatus('Adding folder...');
    try {
      const response = await endpoints.addFolder(path);
      if (clearInput) setFolderInput('');
      const nextFolders = response.folders || [];
      setFolders(nextFolders);
      setFolderStatus(nextFolders.length ? '' : 'No folders added yet.');
    } catch (error) {
      setFolderStatus(error instanceof Error ? error.message : 'Could not add folder');
    }
  };

  const addFolder = async () => {
    await addFolderPath(folderInput.trim(), true);
  };

  const pickFolder = async () => {
    if (isPickingFolder) return;
    setIsPickingFolder(true);
    setFolderStatus('Opening folder picker...');
    try {
      const response = await endpoints.pickFolder();
      const path = String(response.path || '').trim();
      if (!path) {
        setFolderStatus(folders.length ? '' : 'No folders added yet.');
        return;
      }
      setFolderInput(path);
      await addFolderPath(path, true);
    } catch (error) {
      setFolderStatus(error instanceof Error ? error.message : 'Could not choose folder');
    } finally {
      setIsPickingFolder(false);
    }
  };

  const removeFolder = async (path: string) => {
    if (!path || removingFolder) return;
    setRemovingFolder(path);
    setFolderStatus('Removing folder...');
    try {
      const response = await endpoints.removeFolder(path);
      const nextFolders = response.folders || [];
      setFolders(nextFolders);
      setFolderStatus(nextFolders.length ? '' : 'No folders added yet.');
    } catch (error) {
      const message = error instanceof Error ? error.message : 'Could not remove folder';
      setFolderStatus(message);
      throw error;
    } finally {
      setRemovingFolder('');
    }
  };

  const rescan = async () => {
    if (isScanning) return;
    setIsScanning(true);
    setScanStatus('Preparing scan...');
    setScanProgress({
      running: true,
      phase: 'preparing',
      scanned: 0,
      total: 0,
      updated: 0,
      removed: 0,
      currentPath: '',
      message: 'Preparing scan...',
      error: ''
    });
    stopScanPolling();
    try {
      const started = applyScanProgress(await endpoints.rescan());
      setIsScanning(started.running);
      let completed = false;
      const pollUntilDone = async () => {
        if (completed) return;
        const progress = await pollScanProgress();
        if (!progress || completed) return;
        setIsScanning(progress.running);
        if (!progress.running) {
          completed = true;
          stopScanPolling();
          await onRefresh();
        }
      };
      if (started.running) {
        void pollUntilDone();
        scanPollRef.current = window.setInterval(() => {
          void pollUntilDone();
        }, 450);
      } else {
        await onRefresh();
      }
    } catch {
      setScanStatus('Scan failed');
      setScanProgress((progress) =>
        progress
          ? {
              ...progress,
              running: false,
              phase: 'error',
              error: 'Scan failed',
              message: 'Scan failed',
              currentPath: ''
            }
          : null
      );
      stopScanPolling();
      setIsScanning(false);
    }
  };

  return {
    addFolder,
    folderInput,
    folderStatus,
    folders,
    isPickingFolder,
    isScanning,
    pickFolder,
    removeFolder,
    removingFolder,
    rescan,
    scanProgress,
    scanStatus,
    setFolderInput
  };
}

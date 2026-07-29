import { useCallback, useEffect, useRef, useState } from 'react';
import { appleMusicBlockedMessage } from '../../features/playback/model/appleMusicStream';
import type { JsonRecord } from '../../shared/types';
import type { ToolbarAction } from '../../shared/ui/toolbar';

const NOTICE_TIMEOUT_MS = 3000;

function shouldSuppressNotice(message: string) {
  return message.trim().toLowerCase() === 'playback changed';
}

export function useAppNotices() {
  const [notice, setNotice] = useState('');
  const [noticeKey, setNoticeKey] = useState(0);
  const [alert, setAlert] = useState('');
  const [toolbarAction, setToolbarAction] = useState<ToolbarAction | null>(null);
  const noticeTimeoutRef = useRef<number | null>(null);
  // Every failure arrives through the same `setNotice` the whole app already
  // holds, but naming the output that blocked a request needs status this hook
  // is created before. The latest status is parked here instead of threading a
  // second reporter through every caller.
  const statusRef = useRef<JsonRecord | null>(null);

  const clearNoticeTimeout = useCallback(() => {
    if (noticeTimeoutRef.current !== null) {
      window.clearTimeout(noticeTimeoutRef.current);
      noticeTimeoutRef.current = null;
    }
  }, []);

  useEffect(() => clearNoticeTimeout, [clearNoticeTimeout]);

  const setAlertStatus = useCallback((status: JsonRecord | null | undefined) => {
    statusRef.current = status ?? null;
  }, []);

  const dismissAlert = useCallback(() => setAlert(''), []);

  const showNotice = useCallback(
    (message: string) => {
      clearNoticeTimeout();
      if (!message || shouldSuppressNotice(message)) {
        setNotice('');
        return;
      }

      // A refusal the listener has to resolve elsewhere outlives a three-second
      // toast, so it is promoted to the banner instead of flashing past.
      const blocked = appleMusicBlockedMessage(message, statusRef.current);
      if (blocked) {
        setNotice('');
        setAlert(blocked);
        return;
      }

      setNotice(message);
      setNoticeKey((key) => key + 1);
      noticeTimeoutRef.current = window.setTimeout(() => {
        setNotice('');
        noticeTimeoutRef.current = null;
      }, NOTICE_TIMEOUT_MS);
    },
    [clearNoticeTimeout]
  );

  return {
    alert,
    dismissAlert,
    notice,
    noticeKey,
    setAlertStatus,
    setNotice: showNotice,
    setToolbarAction,
    toolbarAction
  };
}

import { useCallback, useEffect, useRef, useState } from 'react';
import type { ToolbarAction } from '../../shared/ui/toolbar';

const NOTICE_TIMEOUT_MS = 3000;

function shouldSuppressNotice(message: string) {
  return message.trim().toLowerCase() === 'playback changed';
}

export function useAppNotices() {
  const [notice, setNotice] = useState('');
  const [noticeKey, setNoticeKey] = useState(0);
  const [toolbarAction, setToolbarAction] = useState<ToolbarAction | null>(null);
  const noticeTimeoutRef = useRef<number | null>(null);

  const clearNoticeTimeout = useCallback(() => {
    if (noticeTimeoutRef.current !== null) {
      window.clearTimeout(noticeTimeoutRef.current);
      noticeTimeoutRef.current = null;
    }
  }, []);

  useEffect(() => clearNoticeTimeout, [clearNoticeTimeout]);

  const showNotice = useCallback(
    (message: string) => {
      clearNoticeTimeout();
      if (!message || shouldSuppressNotice(message)) {
        setNotice('');
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
    notice,
    noticeKey,
    setNotice: showNotice,
    setToolbarAction,
    toolbarAction
  };
}

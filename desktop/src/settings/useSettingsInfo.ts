import { useCallback, useEffect, useRef, useState } from 'react';
import { type SettingsInfo, settingsInfo } from '../api';
import { attempt } from '../recording/attempt';

export type InfoView = { kind: 'loading' } | { kind: 'failed'; message: string } | { kind: 'info'; info: SettingsInfo };

/** `settings_info` on mount and on `reload()`; the latest request wins. */
export function useSettingsInfo(): { view: InfoView; reload: () => void } {
  const [view, setView] = useState<InfoView>({ kind: 'loading' });
  const active = useRef(true);
  const requests = useRef(0);

  const reload = useCallback(async () => {
    const id = ++requests.current;
    const reply = await attempt(settingsInfo);
    if (!active.current || id !== requests.current) return;
    if (reply.ok) {
      const { recordings_dir, vault_dir, vault_folder, store } = reply.reply;
      setView({ kind: 'info', info: { recordings_dir, vault_dir, vault_folder, store } });
    } else {
      setView({ kind: 'failed', message: reply.text });
    }
  }, []);

  useEffect(() => {
    active.current = true;
    reload();
    return () => {
      active.current = false;
    };
  }, [reload]);

  return { view, reload };
}

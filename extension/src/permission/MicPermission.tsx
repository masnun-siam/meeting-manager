import { useState } from 'react';
import styles from './MicPermission.module.css';

type Result = 'idle' | 'granted' | 'blocked' | { error: string };

function message(result: Result): string {
  if (result === 'idle') return '';
  if (result === 'granted') return 'Microphone allowed. Go back to your meeting tab and press Start again.';
  if (result === 'blocked') {
    return "Microphone blocked. Recordings will include tab audio only. To change this, allow the microphone for Meeting Manager in Chrome's site settings.";
  }
  return `Could not get the microphone: ${result.error}`;
}

export function MicPermission() {
  const [result, setResult] = useState<Result>('idle');

  async function allow() {
    try {
      const stream = await navigator.mediaDevices.getUserMedia({ audio: true });
      stream.getTracks().forEach((t) => {
        t.stop();
      });
      setResult('granted');
    } catch (error) {
      if (error instanceof DOMException && error.name === 'NotAllowedError') setResult('blocked');
      else setResult({ error: error instanceof Error ? error.message : String(error) });
    }
  }

  return (
    <main className={styles.page}>
      <h1>Allow the microphone</h1>
      <p>Meeting Manager needs your microphone so your voice is in recordings.</p>
      <button className={styles.button} type="button" disabled={result === 'granted'} onClick={allow}>
        Allow microphone
      </button>
      <p role="status">{message(result)}</p>
    </main>
  );
}

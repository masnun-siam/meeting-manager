function pad(n: number): string {
  return String(n).padStart(2, '0');
}

/** `H:MM:SS`, e.g. `0:04:01`. */
export function formatElapsed(ms: number): string {
  const total = Math.floor(ms / 1000);
  return `${Math.floor(total / 3600)}:${pad(Math.floor((total % 3600) / 60))}:${pad(total % 60)}`;
}

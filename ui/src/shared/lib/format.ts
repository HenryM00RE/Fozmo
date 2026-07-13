export function formatTime(seconds: number | null | undefined) {
  const safeSeconds = Math.max(0, Number(seconds) || 0);
  const minutes = Math.floor(safeSeconds / 60);
  const remainingSeconds = Math.floor(safeSeconds % 60);
  return `${minutes}:${String(remainingSeconds).padStart(2, '0')}`;
}

export function stripFileExtension(name: string | null | undefined) {
  return String(name || '').replace(/\.[^.]+$/, '');
}

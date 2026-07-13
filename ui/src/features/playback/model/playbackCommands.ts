import { api } from '../../../shared/lib/api';

async function postCommand(endpoint: string, body?: unknown) {
  await api.post(endpoint, body);
}

export function pausePlayback() {
  return postCommand('/api/pause');
}

export function resumePlayback() {
  return postCommand('/api/resume');
}

export function stopPlayback() {
  return postCommand('/api/stop');
}

export function seekPlayback(seconds: number) {
  return postCommand('/api/seek', { seconds });
}

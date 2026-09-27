/**
 * Base64 helpers for PTY traffic.
 *
 * Tauri event/command payloads are JSON, so raw bytes would travel as an array
 * of numbers (one JSON token per byte). Base64 keeps the same payload roughly
 * a third larger than the bytes instead of four times larger, and costs a
 * single decode instead of an array -> typed-array copy.
 */

const BIN_CHUNK = 0x8000;

export function base64ToBytes(value: string): Uint8Array {
  if (!value) return new Uint8Array(0);
  const binary = atob(value);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) {
    bytes[i] = binary.charCodeAt(i);
  }
  return bytes;
}

export function bytesToBase64(bytes: Uint8Array): string {
  let binary = '';
  for (let i = 0; i < bytes.length; i += BIN_CHUNK) {
    // fromCharCode wants individual arguments; chunk to stay off the stack limit.
    const chunk = bytes.subarray(i, Math.min(i + BIN_CHUNK, bytes.length));
    binary += String.fromCharCode(...chunk);
  }
  return btoa(binary);
}

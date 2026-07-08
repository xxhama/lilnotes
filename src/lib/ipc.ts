/**
 * Typed wrappers around Tauri IPC.
 *
 * Every backend command gets a thin, typed function here so views never call
 * `invoke` with raw strings scattered around the codebase.
 */
import { invoke } from "@tauri-apps/api/core";

/** Response of the `ping` health-check command. */
export interface PingResponse {
  /** Echo of the message that was sent. */
  echo: string;
  /** Backend crate version. */
  version: string;
  /** Unix epoch milliseconds when the backend handled the call. */
  handledAtMs: number;
}

/** Round-trip a message through the Rust backend. */
export function ping(message: string): Promise<PingResponse> {
  return invoke<PingResponse>("ping", { message });
}

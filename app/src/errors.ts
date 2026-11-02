// Errors rejected by the Tauri commands, made readable for the UI (§6).

import { ErrorOut } from "./ipc";

/// DB commands reject with a serialised ErrorOut; everything else becomes
/// a readable internal error.
export function asErrorOut(e: unknown): ErrorOut {
  if (e && typeof e === "object" && "message" in e && "severity" in e) return e as ErrorOut;
  return {
    severity: "error",
    category: "internal",
    code: null,
    message: String(e),
    detail: null,
    hint: null,
    context: null,
    position_char: null,
    internal_query: null,
    statement_char_offset: null,
    statement_index: null,
  };
}

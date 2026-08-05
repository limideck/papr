// Desktop auto-update was Tauri-only. Web builds have nothing to check —
// keep the export so Settings/About call sites compile without branching.

export async function checkForUpdates(_opts: { silent: boolean }): Promise<void> {
  // no-op on web
}

/*
 * disclaim — exec a binary as its own TCC "responsible process".
 *
 * macOS attributes privacy permissions (TCC) to the *responsible process* of
 * whoever asks. Anything spawned from a terminal pipeline (cargo run, npm,
 * bash) inherits the terminal as its responsible process, so a System Audio
 * Recording request from the dev app is evaluated against the TERMINAL —
 * which has no NSAudioCaptureUsageDescription — and is silently denied:
 * the Core Audio process tap comes up but delivers only zeros. Launching the
 * same bundle via Finder/`open` works because LaunchServices makes the app
 * responsible for itself.
 *
 * This shim reproduces that with `responsibility_spawnattrs_setdisclaim`,
 * the same (private, dev-tooling-only — never ship it) API Chromium and
 * VS Code use for their helper processes. POSIX_SPAWN_SETEXEC makes it an
 * exec-in-place: pid, stdio, and signal handling are preserved, so the
 * tauri dev watcher and Ctrl-C keep working exactly as with a plain exec.
 *
 * Compiled on demand by scripts/macos-dev-runner.sh; not part of the app.
 */
#include <spawn.h>
#include <stdio.h>

extern int responsibility_spawnattrs_setdisclaim(posix_spawnattr_t *attrs, int disclaim);
extern char **environ;

int main(int argc, char **argv) {
    if (argc < 2) {
        fprintf(stderr, "usage: disclaim <binary> [args...]\n");
        return 2;
    }
    posix_spawnattr_t attr;
    posix_spawnattr_init(&attr);
    responsibility_spawnattrs_setdisclaim(&attr, 1);
    posix_spawnattr_setflags(&attr, POSIX_SPAWN_SETEXEC);
    posix_spawn(NULL, argv[1], NULL, &attr, argv + 1, environ);
    perror("disclaim: posix_spawn");
    return 127;
}

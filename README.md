# Update Notifier

A small Windows system-tray app that notifies you when GitHub repositories get a new release or new commits.

- Per repository, choose what to watch: **stable releases**, **all releases including pre-releases**, or **new commits on a branch**.
- Add, edit and remove repositories from the tray menu. Paste `owner/name` or any GitHub URL. For commit watching, branches load into a drop-down (default branch first); you can also type any branch name.
- Native Windows notifications; clicking one opens the release or compare page. Unseen updates stay in the menu and badge the tray icon until opened or dismissed.
- The first check of a repo only records a baseline, so you are never notified about things that already existed.
- Configurable check interval (15 min – 12 h), "Check now", start with Windows.
- Menu follows the Windows light/dark mode.
- Single ~3 MB executable, no runtime dependencies. Raw Win32 via `windows-sys`, HTTP via `ureq` (rustls with the Windows certificate store).

## Build

Requires Rust (MSVC toolchain) on Windows 10 1903+ / Windows 11.

```
cargo build --release
```

The binary is `target\release\update-notifier.exe`.

## GitHub authentication

Without a token GitHub allows 60 API requests per hour, and every check costs one request per repository. With a token the limit is 5,000/hour, and repositories that haven't changed cost nothing ([conditional requests](https://docs.github.com/en/rest/using-the-rest-api/best-practices-for-using-the-rest-api#use-conditional-requests-if-appropriate) returning `304` are free only when authenticated).

If you are already signed in, nothing needs to be configured. The token is looked up in this order, re-checked before every round of checks:

1. A token saved from the tray menu (**GitHub token…**), stored in Windows Credential Manager.
2. The `GITHUB_TOKEN` or `GH_TOKEN` environment variable.
3. The GitHub CLI login (`gh auth token`).
4. Git's credential helpers for github.com (`git credential fill`, e.g. Git Credential Manager).

The `gh` and `git` lookups run without a console window and can never prompt: Git Credential Manager is run with `credential.interactive=false`, and Git with an empty `GIT_ASKPASS` and `GIT_TERMINAL_PROMPT=0`. The app only makes read-only `GET` requests, but note that a `gh`/Git login token usually has broad scopes; save a fine-grained token with no extra permissions if you prefer a narrower one.

## Files

- Config and state: `%APPDATA%\UpdateNotifier\config.json`
- Startup entry: `HKCU\Software\Microsoft\Windows\CurrentVersion\Run\UpdateNotifier`

## Notes

- Release "latest" follows GitHub's definition (`/releases/latest`: newest non-draft, non-pre-release).
- Rolling tags that are re-published under the same name (e.g. a `nightly` tag) do not trigger notifications.
- Dark menus use undocumented `uxtheme.dll` exports (ordinals 135/136), the same approach used by Explorer and Notepad++.

## Tests

```
cargo test                      # offline unit tests
cargo test -- --ignored         # live tests against the GitHub API
```

# Update Notifier

A small Windows system-tray app that notifies you when GitHub repositories get a new release or new commits.

- Per repository, choose what to watch: **stable releases**, **all releases including pre-releases**, or **new commits on a branch**.
- Add, edit and remove repositories from the tray menu. Paste `owner/name` or any GitHub URL.
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

## GitHub token (recommended)

Without a token GitHub allows 60 API requests per hour, and every check costs one request per repository. With a token the limit is 5,000/hour, and repositories that haven't changed cost nothing ([conditional requests](https://docs.github.com/en/rest/using-the-rest-api/best-practices-for-using-the-rest-api#use-conditional-requests-if-appropriate) returning `304` are free only when authenticated).

Set it from the tray menu (**GitHub token…**). A fine-grained token with no extra permissions is enough for public repositories. It is stored in Windows Credential Manager. The `GITHUB_TOKEN` environment variable is used as a fallback.

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

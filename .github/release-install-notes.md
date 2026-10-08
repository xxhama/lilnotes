## Installation

1. Download `LilNotes_*_aarch64.dmg` below (Apple Silicon, macOS 15+ only).
2. Open the DMG and drag **LilNotes** to Applications.

### First launch (important)

LilNotes is not yet notarized by Apple, so macOS will refuse to open
it with a "damaged" or "cannot be opened" warning. Two ways past it:

- Run this once in Terminal, then launch normally:

  ```sh
  xattr -cr /Applications/LilNotes.app
  ```

- Or: try to open the app, then go to **System Settings → Privacy &
  Security**, scroll down, and click **Open Anyway**.

### Verify your download (optional)

Check the checksum:

```sh
shasum -a 256 -c checksums.txt
```

Or confirm the DMG was built by this repository's release workflow
(needs the [GitHub CLI](https://cli.github.com)):

```sh
gh attestation verify LilNotes_*_aarch64.dmg -R xxhama/lilnotes
```

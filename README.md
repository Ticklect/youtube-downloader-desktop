# YouTube Downloader Desktop

**Windows x64 | Beta | Free and open source**

A Windows app for finding videos on a public YouTube channel, choosing what to save, and downloading permitted videos, audio, or available captions. It runs on your PC; you do not need a browser extension, Python, or an account with this project.

[![Watch YouTube Downloader Desktop in action — 20-second demo](docs/media/desktop-demo-poster.jpg)](https://github.com/Ticklect/youtube-downloader-desktop/releases/download/desktop-beta-lazy-loading/YouTube-Downloader-Desktop-Demo.mp4)

**[Watch or download the 20-second demo](https://github.com/Ticklect/youtube-downloader-desktop/releases/download/desktop-beta-lazy-loading/YouTube-Downloader-Desktop-Demo.mp4)** | **[Download the Desktop Beta](https://github.com/Ticklect/youtube-downloader-desktop/releases/latest)** | [See download options](docs/screenshots/options.png)

The demo shows selecting videos from a channel, choosing video or audio formats and download quality, and managing downloads in the native Windows app.

## Install

1. Open the [latest Desktop Beta release](https://github.com/Ticklect/youtube-downloader-desktop/releases/latest) and download the standalone Windows x64 EXE.
2. Run **`youtubedownloderbeta.exe`**. The standalone build embeds yt-dlp, FFmpeg, FFprobe, and Deno; you do not need to download those tools or keep a `bin` folder beside the EXE.
3. Keep the accompanying third-party notices file with the EXE; it contains the bundled tools' license texts and attributions.

On first launch, the app extracts the embedded tools into a versioned directory under `%LOCALAPPDATA%\YouTube Downloader Desktop\tools\`. This is separate from the saved session and thumbnail cache, so updating or extracting the runtime does not reset your settings, saved channels, selected videos, or download history. The app does not require a separate installation of Rust, Python, FFmpeg, or yt-dlp.

The Beta is currently **Windows x64 only**. It has no installer and is not Authenticode-signed, so Windows may show a publisher or reputation warning. Only run an EXE obtained from a source you trust and verify its published SHA-256 when one is provided. Signing would require a trusted code-signing certificate; bundling dependencies does not remove Windows trust warnings.

Older Windows x64 ZIP packages contain the EXE and a separate `bin` directory. They are legacy portable packages and are **not** the standalone single-EXE build described above.

## How to use it

1. Copy a public YouTube channel address, such as `https://www.youtube.com/@ChannelName/videos`.
2. Paste it into the **Library** page and click **Load channel**.
3. After browsing channels, use **From your searches** to add a channel to the **Saved channels** bar. Open a saved channel again in one click, or remove it from the bar. Suggestions come from successfully loaded channels on this computer and are ranked from your search history; they are not endorsements or live YouTube recommendations.
4. Tick the videos you want to save. Use the search box to narrow the list.
5. Click **Continue**, then choose the download folder, format, and quality.
6. Start the downloads. Open **Downloads** to check progress, open finished files, cancel active jobs, or retry failures.

You can also switch between the **YouTube Red** and **Mono** themes. Theme and fullscreen controls are in the compact footer. The app remembers your current channel, selected videos, saved channels, settings, and download history between launches.

## Screenshots

**Library — browse a channel and select the videos you want:**

![YouTube Downloader Desktop Beta browsing 259 videos from a public channel](docs/screenshots/library.png)

| Download options | Downloads and history |
| :---: | :---: |
| ![Choose video quality, audio, captions, and destination](docs/screenshots/options.png) | ![View completed downloads, failed jobs, and retry controls](docs/screenshots/downloads.png) |

**Mono theme** — the same Library with a neutral colour scheme:

![YouTube Downloader Desktop Beta in Mono theme](docs/screenshots/mono-library.png)

The main Library screenshot shows a real public channel with **259 videos loaded** during pagination testing. The other screenshots use isolated example data. These screenshots and the demo show an earlier Beta layout; the current Beta has a smaller top navigation area and a saved channels shelf.

## What it supports

| Feature | Current Beta |
| --- | --- |
| Channel browsing | First 200 public videos immediately; scrolling near the bottom loads another 200, up to 1,000 |
| Channel shortcuts | Suggested channels based on your local successful channel searches; pin to or remove from the saved channels bar |
| Video | Best available, 1080p, 720p, or 360p, when available |
| Audio | MP3 output |
| Captions | English captions, if available for the video |
| Downloads | Folder selection, progress, cancellation, retry, and history |
| Processing | yt-dlp, FFmpeg, FFprobe, and Deno embedded in the standalone EXE and extracted locally on first launch |

Some uploads, formats, or captions may not be available. YouTube can change how videos are delivered, so not every download will succeed. Private, members-only, login-restricted, and DRM-protected content are not supported. This is a **Beta**, not a guarantee of uninterrupted downloads.

To browse older uploads, keep scrolling down the **Library** list. The app loads more in batches instead of scanning the full channel upfront. Your selections remain checked while the next batch loads; if a request fails, use **Retry loading** at the end of the list.

## Responsible use and legal notice

**Only use this software to download material you are permitted to download.** For example, this may include videos you created, material for which you have the rights holder's permission, or content whose licence or applicable law permits downloading. Check the actual permissions before downloading: **a video being publicly viewable does not automatically mean you have permission to save or redistribute it.**

You are responsible for your own use of the software, including complying with copyright law, privacy rights, applicable regulations, and the terms of any platform or service you access. Do not use it to infringe someone else's rights or bypass DRM, paywalls, authentication, or other access restrictions. Downloading a file does not grant permission to republish or distribute it.

**Disclaimer:** This project is provided **"as is"**, without warranties, as described in the [AGPL-3.0 licence](LICENSE). To the extent permitted by applicable law, the authors and maintainers disclaim liability for misuse of the software, infringement by users, lost data, service restrictions, or other damage arising from its use. **This notice does not automatically remove legal liability or override rights and obligations that cannot legally be excluded.**

This is an independent project. It is **not affiliated with, sponsored by, or endorsed by YouTube or Google**. YouTube and Google names and marks belong to their respective owners.

## For developers

The desktop app is written in **Rust** and uses **Slint** for its native UI. On Windows, install stable Rust with the MSVC toolchain and Windows SDK/C++ build tools, then run:

```powershell
cargo fmt --all -- --check
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/package-windows.ps1 -Tag Local-Build
```

The packaging script downloads and checksum-verifies the upstream tool assets, freshly extracts the verified archives, builds a raw-DEFLATE payload, and passes its path and SHA-256 into a required embedded-runtime release build. It creates a standalone EXE and a `THIRD-PARTY-NOTICES Local-Build.txt` file under `dist/`. Output names are immutable: the script refuses to overwrite an existing output, so use a new `-Tag` for each package. Packaging requires network access to the upstream release and checksum URLs.

To run the Windows GUI smoke tests against that EXE:

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts/smoke-beta-acceptance.ps1 -Executable 'dist\YouTube Downloader Desktop Beta Local-Build.exe' -CaptureMatrix
```

Those tests use isolated sample data, not your real downloaded files. Your regular app settings and history are stored under `%LOCALAPPDATA%\YouTube Downloader Desktop\`.

## Other versions and licences

A separate **browser-only extension** is being developed as an alternative for people who do not want a desktop app. It is experimental, has different download limitations, and is **not included in this repository or release**. The old extension-plus-Python-helper project is retired; its source remains in Git history, not in the current desktop codebase.

This project's source is licensed under **[AGPL-3.0](LICENSE)**. The UI uses [Slint](https://slint.dev/) and includes its attribution under **About & legal**. The standalone package accompanies the complete project license and license texts for yt-dlp, FFmpeg, and Deno in its third-party notices file. See [third-party notices and upstream sources](docs/THIRD-PARTY-NOTICES.md) for attribution and license source links.

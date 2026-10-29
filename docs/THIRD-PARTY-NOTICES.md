# Third-party notices

The standalone Windows EXE embeds four upstream runtime tools. The packaging script includes their license text in the accompanying `THIRD-PARTY-NOTICES.txt` file; retain that file when redistributing the EXE.

| Component | Purpose | Upstream project and license source |
| --- | --- | --- |
| yt-dlp | Extracts public video and audio metadata and performs downloads | [yt-dlp](https://github.com/yt-dlp/yt-dlp), [license text](https://github.com/yt-dlp/yt-dlp/blob/master/LICENSE) |
| FFmpeg | Media conversion and processing | [FFmpeg](https://ffmpeg.org/), license text supplied with the verified [Gyan Windows essentials build](https://www.gyan.dev/ffmpeg/builds/) |
| FFprobe | Media stream and format inspection; distributed with FFmpeg | [FFmpeg](https://ffmpeg.org/), license text supplied with the verified Gyan Windows essentials build |
| Deno | JavaScript runtime used by yt-dlp | [Deno](https://deno.com/), [license text](https://github.com/denoland/deno/blob/main/LICENSE.md) |
| Slint | Native application UI framework | [Slint](https://slint.dev/); attribution and legal information are also shown in the application's **About & legal** section |

The application source is licensed under [GNU AGPL version 3](../LICENSE). Its complete license text is included in the generated third-party notices file. The build script fetches the yt-dlp and Deno license texts from their upstream repositories and validates expected license markers; it extracts the FFmpeg license text from the same checksum-verified archive used to obtain the FFmpeg executables.

Upstream runtime tools can change between builds because the packaging script uses their current official release URLs. Each build verifies the downloaded tool assets against the checksum files published at those URLs. Review the generated notice file for the exact license text distributed with a particular EXE.

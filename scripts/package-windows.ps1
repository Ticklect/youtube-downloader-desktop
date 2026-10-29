param([string]$Tag = '')

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$root = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$tools = Join-Path $root 'tools'
$downloads = Join-Path $tools 'downloads'
$bin = Join-Path $tools 'bin'
$dist = Join-Path $root 'dist'

if ($Tag -and $Tag -notmatch '^[A-Za-z0-9][A-Za-z0-9._-]{0,39}$') {
    throw 'Package tag must be 1-40 ASCII letters, digits, periods, underscores or hyphens.'
}
$suffix = if ($Tag) { ' ' + $Tag } else { '' }
$outputName = 'YouTube Downloader Desktop Beta' + $suffix
$outputExe = Join-Path $dist ($outputName + '.exe')
$outputNotices = Join-Path $dist ('THIRD-PARTY-NOTICES' + $suffix + '.txt')

# Final artifacts are immutable. Check before downloading or building so a
# repeated tag cannot replace a release binary or its notices.
foreach ($path in @($outputExe, $outputNotices)) {
    if (Test-Path -LiteralPath $path) {
        throw "Output already exists; choose a new -Tag. Existing output was preserved: $path"
    }
}

foreach ($directory in @($tools, $downloads, $bin, $dist)) {
    New-Item -ItemType Directory -Path $directory -Force | Out-Null
}
$stage = Join-Path $tools ('standalone-stage-' + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $stage | Out-Null
$preserveStageOnFailure = $false

function Get-ExpectedSha256([string]$content, [string]$assetName) {
    if ($assetName) {
        $matching = @($content -split '\r?\n' | Where-Object {
            $_ -match ('(?i)^[0-9a-f]{64}\s+\*?' + [regex]::Escape($assetName) + '$')
        })
        if ($matching.Count -ne 1) { throw "Missing exactly one checksum for $assetName" }
        return [regex]::Match($matching[0], '[0-9a-fA-F]{64}').Value.ToUpperInvariant()
    }

    $digestMatches = [regex]::Matches($content, '(?i)(?<![0-9a-f])[0-9a-f]{64}(?![0-9a-f])')
    if ($digestMatches.Count -ne 1) { throw 'Checksum response must contain exactly one SHA-256 digest.' }
    return $digestMatches[0].Value.ToUpperInvariant()
}

function Verified-Download(
    [string]$downloadUri,
    [string]$checksumUri,
    [string]$destination,
    [string]$assetName
) {
    $checksumResponse = Invoke-WebRequest -UseBasicParsing -Uri $checksumUri -TimeoutSec 45
    $checksumContent = if ($checksumResponse.Content -is [byte[]]) {
        [Text.Encoding]::UTF8.GetString($checksumResponse.Content)
    } else {
        [string]$checksumResponse.Content
    }
    $expected = Get-ExpectedSha256 $checksumContent $assetName
    if ($expected.Length -ne 64) { throw "Checksum unavailable for $downloadUri" }

    $cachedHash = if (Test-Path -LiteralPath $destination -PathType Leaf) {
        (Get-FileHash -LiteralPath $destination -Algorithm SHA256).Hash.ToUpperInvariant()
    } else { '' }
    if ($cachedHash -ne $expected) {
        $temporaryDownload = $destination + '.download-' + [Guid]::NewGuid().ToString('N')
        try {
            Write-Host "Downloading $downloadUri"
            Invoke-WebRequest -UseBasicParsing -Uri $downloadUri -OutFile $temporaryDownload -TimeoutSec 300
            $downloadHash = (Get-FileHash -LiteralPath $temporaryDownload -Algorithm SHA256).Hash.ToUpperInvariant()
            if ($downloadHash -ne $expected) { throw "SHA256 mismatch for downloaded asset: $downloadUri" }
            if (Test-Path -LiteralPath $destination) { Remove-Item -LiteralPath $destination -Force }
            Move-Item -LiteralPath $temporaryDownload -Destination $destination
        } finally {
            if (Test-Path -LiteralPath $temporaryDownload) { Remove-Item -LiteralPath $temporaryDownload -Force }
        }
    }

    $actual = (Get-FileHash -LiteralPath $destination -Algorithm SHA256).Hash.ToUpperInvariant()
    if ($actual -ne $expected) { throw "SHA256 mismatch after verification: $destination" }
    Write-Host "Verified SHA-256: $destination ($actual)"
}

function Download-LicenseText([string]$uri, [string]$destination, [string]$requiredText) {
    $response = Invoke-WebRequest -UseBasicParsing -Uri $uri -TimeoutSec 45
    $content = [string]$response.Content
    if ([string]::IsNullOrWhiteSpace($content) -or $content.Length -lt 100 -or
        $content -notmatch $requiredText -or $content -match '(?i)^\s*<!doctype html') {
        throw "The upstream license text did not pass validation: $uri"
    }
    [IO.File]::WriteAllText($destination, $content.TrimEnd() + "`r`n", [Text.UTF8Encoding]::new($false))
}

function Assert-WindowsExecutable([string]$path) {
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { throw "Required runtime executable is missing: $path" }
    $info = Get-Item -LiteralPath $path
    if ($info.Length -lt 4096) { throw "Runtime executable is implausibly small: $path" }
    $stream = [IO.File]::OpenRead($path)
    try {
        if ($stream.ReadByte() -ne 0x4D -or $stream.ReadByte() -ne 0x5A) {
            throw "Runtime file does not have a Windows PE MZ header: $path"
        }
    } finally { $stream.Dispose() }
}

function Convert-Sha256ToBytes([string]$hex) {
    if ($hex -notmatch '^(?i:[0-9a-f]{64})$') { throw 'Invalid SHA-256 hexadecimal value.' }
    [byte[]]$bytes = for ($index = 0; $index -lt 32; $index++) {
        [Convert]::ToByte($hex.Substring($index * 2, 2), 16)
    }
    return ,$bytes
}

function New-EmbeddedRuntimePayload([string]$sourceDirectory, [string]$payloadPath, [string]$compressionDirectory) {
    $entries = @(
        @{ Name = 'yt-dlp.exe'; Path = (Join-Path $sourceDirectory 'yt-dlp.exe') },
        @{ Name = 'ffmpeg.exe'; Path = (Join-Path $sourceDirectory 'ffmpeg.exe') },
        @{ Name = 'ffprobe.exe'; Path = (Join-Path $sourceDirectory 'ffprobe.exe') },
        @{ Name = 'deno.exe'; Path = (Join-Path $sourceDirectory 'deno.exe') }
    )
    New-Item -ItemType Directory -Path $compressionDirectory | Out-Null

    $prepared = @()
    foreach ($entry in $entries) {
        Assert-WindowsExecutable $entry.Path
        $before = Get-Item -LiteralPath $entry.Path
        $hashBefore = (Get-FileHash -LiteralPath $entry.Path -Algorithm SHA256).Hash.ToUpperInvariant()
        $compressedPath = Join-Path $compressionDirectory ($entry.Name + '.deflate')
        $inputStream = $null
        $compressedStream = $null
        $deflateStream = $null
        try {
            $inputStream = [IO.File]::OpenRead($entry.Path)
            $compressedStream = [IO.File]::Create($compressedPath)
            # DeflateStream writes a raw RFC 1951 DEFLATE stream (no zlib/gzip wrapper).
            $deflateStream = [IO.Compression.DeflateStream]::new(
                $compressedStream, [IO.Compression.CompressionLevel]::Optimal, $true
            )
            $inputStream.CopyTo($deflateStream)
        } finally {
            if ($deflateStream) { $deflateStream.Dispose() }
            if ($inputStream) { $inputStream.Dispose() }
            if ($compressedStream) { $compressedStream.Dispose() }
        }

        $after = Get-Item -LiteralPath $entry.Path
        $hashAfter = (Get-FileHash -LiteralPath $entry.Path -Algorithm SHA256).Hash.ToUpperInvariant()
        if ($before.Length -ne $after.Length -or $hashBefore -ne $hashAfter) {
            throw "Runtime input changed while it was being packaged: $($entry.Name)"
        }
        $compressedInfo = Get-Item -LiteralPath $compressedPath
        if ($compressedInfo.Length -eq 0) { throw "Compression produced an empty stream: $($entry.Name)" }
        $prepared += @{
            Name = $entry.Name
            Path = $entry.Path
            CompressedPath = $compressedPath
            RawLength = [uint64]$after.Length
            CompressedLength = [uint64]$compressedInfo.Length
            Hash = $hashAfter
        }
    }

    $output = $null
    $writer = $null
    try {
        $output = [IO.File]::Create($payloadPath)
        $writer = [IO.BinaryWriter]::new($output, [Text.Encoding]::UTF8, $true)
        $writer.Write([Text.Encoding]::ASCII.GetBytes('YTDPLD01'))
        $writer.Write([uint32]$prepared.Count)
        foreach ($entry in $prepared) {
            $nameBytes = [Text.Encoding]::UTF8.GetBytes($entry.Name)
            if ($nameBytes.Length -gt [uint16]::MaxValue) { throw 'Runtime name is too long.' }
            $writer.Write([uint16]$nameBytes.Length)
            $writer.Write($nameBytes)
            $writer.Write([uint64]$entry.RawLength)
            $writer.Write([uint64]$entry.CompressedLength)
            $writer.Write((Convert-Sha256ToBytes $entry.Hash))
            $writer.Flush()
            $compressedInput = [IO.File]::OpenRead($entry.CompressedPath)
            try { $compressedInput.CopyTo($output) } finally { $compressedInput.Dispose() }
        }
        $writer.Flush()
    } finally {
        if ($writer) { $writer.Dispose() }
        if ($output) { $output.Dispose() }
    }

    $payloadInfo = Get-Item -LiteralPath $payloadPath
    if ($payloadInfo.Length -le 12) { throw 'Embedded runtime payload is empty or truncated.' }
    $header = [byte[]]::new(12)
    $payloadInputStream = [IO.File]::OpenRead($payloadPath)
    try {
        if ($payloadInputStream.Read($header, 0, $header.Length) -ne $header.Length) { throw 'Unable to read payload header.' }
    } finally { $payloadInputStream.Dispose() }
    if ([Text.Encoding]::ASCII.GetString($header, 0, 8) -ne 'YTDPLD01') { throw 'Payload magic mismatch.' }
    if ([BitConverter]::ToUInt32($header, 8) -ne 4) { throw 'Payload must contain exactly four runtime tools.' }
    return (Get-FileHash -LiteralPath $payloadPath -Algorithm SHA256).Hash.ToUpperInvariant()
}

function Assert-ExtractedRuntimeMatches([string]$localAppDataRoot, [string]$sourceDirectory) {
    $expectedNames = @('yt-dlp.exe', 'ffmpeg.exe', 'ffprobe.exe', 'deno.exe')
    $foundPaths = @()
    foreach ($name in $expectedNames) {
        $candidates = @(Get-ChildItem -LiteralPath $localAppDataRoot -File -Recurse -Filter $name -ErrorAction SilentlyContinue)
        if ($candidates.Count -ne 1) {
            throw "Runtime self-test expected exactly one extracted $name; found $($candidates.Count)."
        }
        $actual = $candidates[0]
        $expected = Get-Item -LiteralPath (Join-Path $sourceDirectory $name)
        if ($actual.Length -ne $expected.Length) {
            throw "Extracted runtime length mismatch for $name."
        }
        $actualHash = (Get-FileHash -LiteralPath $actual.FullName -Algorithm SHA256).Hash.ToUpperInvariant()
        $expectedHash = (Get-FileHash -LiteralPath $expected.FullName -Algorithm SHA256).Hash.ToUpperInvariant()
        if ($actualHash -ne $expectedHash) { throw "Extracted runtime SHA-256 mismatch for $name." }
        $foundPaths += $actual.FullName
    }

    $parents = @($foundPaths | ForEach-Object { Split-Path -Parent $_ } | Select-Object -Unique)
    if ($parents.Count -ne 1) { throw 'Extracted runtime tools are not all in the same runtime directory.' }
}

try {
    $ytdlpDownload = Join-Path $downloads 'yt-dlp.exe'
    Verified-Download 'https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp.exe' 'https://github.com/yt-dlp/yt-dlp/releases/latest/download/SHA2-256SUMS' $ytdlpDownload 'yt-dlp.exe'

    $ffmpegArchive = Join-Path $downloads 'ffmpeg-release-essentials.7z'
    Verified-Download 'https://www.gyan.dev/ffmpeg/builds/ffmpeg-release-essentials.7z' 'https://www.gyan.dev/ffmpeg/builds/ffmpeg-release-essentials.7z.sha256' $ffmpegArchive ''

    $denoArchive = Join-Path $downloads 'deno-x86_64-pc-windows-msvc.zip'
    Verified-Download 'https://github.com/denoland/deno/releases/latest/download/deno-x86_64-pc-windows-msvc.zip' 'https://github.com/denoland/deno/releases/latest/download/deno-x86_64-pc-windows-msvc.zip.sha256sum' $denoArchive ''

    # Always extract into new staging directories from the checksum-verified
    # archives. Existing tools\bin contents are never trusted as build inputs.
    $denoExtract = Join-Path $stage 'deno-extract'
    $ffmpegExtract = Join-Path $stage 'ffmpeg-extract'
    $verifiedBin = Join-Path $stage 'verified-bin'
    New-Item -ItemType Directory -Path $denoExtract, $ffmpegExtract, $verifiedBin | Out-Null

    Expand-Archive -LiteralPath $denoArchive -DestinationPath $denoExtract
    $denoCandidates = @(Get-ChildItem -LiteralPath $denoExtract -File -Recurse -Filter 'deno.exe')
    if ($denoCandidates.Count -ne 1) { throw 'Verified Deno archive must contain exactly one deno.exe.' }

    & "$env:SystemRoot\System32\tar.exe" -x -f $ffmpegArchive -C $ffmpegExtract
    if ($LASTEXITCODE -ne 0) { throw 'Unable to extract the checksum-verified FFmpeg archive.' }
    $ffmpegCandidates = @(Get-ChildItem -LiteralPath $ffmpegExtract -File -Recurse -Filter 'ffmpeg.exe')
    $ffprobeCandidates = @(Get-ChildItem -LiteralPath $ffmpegExtract -File -Recurse -Filter 'ffprobe.exe')
    if ($ffmpegCandidates.Count -ne 1 -or $ffprobeCandidates.Count -ne 1) {
        throw 'Verified FFmpeg archive must contain exactly one ffmpeg.exe and one ffprobe.exe.'
    }
    $ffmpegLicenseCandidates = @(Get-ChildItem -LiteralPath $ffmpegExtract -File -Recurse | Where-Object { $_.Name -match '^LICENSE(\.txt)?$' })
    if ($ffmpegLicenseCandidates.Count -ne 1) { throw 'Verified FFmpeg archive must contain exactly one LICENSE file.' }

    Copy-Item -LiteralPath $ytdlpDownload -Destination (Join-Path $verifiedBin 'yt-dlp.exe')
    Copy-Item -LiteralPath $ffmpegCandidates[0].FullName -Destination (Join-Path $verifiedBin 'ffmpeg.exe')
    Copy-Item -LiteralPath $ffprobeCandidates[0].FullName -Destination (Join-Path $verifiedBin 'ffprobe.exe')
    Copy-Item -LiteralPath $denoCandidates[0].FullName -Destination (Join-Path $verifiedBin 'deno.exe')
    foreach ($name in @('yt-dlp.exe', 'ffmpeg.exe', 'ffprobe.exe', 'deno.exe')) {
        Assert-WindowsExecutable (Join-Path $verifiedBin $name)
    }

    # Refresh the developer cache from this run's verified staging outputs.
    foreach ($name in @('yt-dlp.exe', 'ffmpeg.exe', 'ffprobe.exe', 'deno.exe')) {
        Copy-Item -LiteralPath (Join-Path $verifiedBin $name) -Destination (Join-Path $bin $name) -Force
        $sourceHash = (Get-FileHash -LiteralPath (Join-Path $verifiedBin $name) -Algorithm SHA256).Hash
        $cachedHash = (Get-FileHash -LiteralPath (Join-Path $bin $name) -Algorithm SHA256).Hash
        if ($sourceHash -ne $cachedHash) { throw "Refreshed runtime cache failed hash comparison: $name" }
    }
    Copy-Item -LiteralPath $ffmpegLicenseCandidates[0].FullName -Destination (Join-Path $bin 'FFmpeg-LICENSE.txt') -Force

    $ytDlpLicense = Join-Path $stage 'yt-dlp-LICENSE.txt'
    $denoLicense = Join-Path $stage 'deno-LICENSE.txt'
    Download-LicenseText 'https://raw.githubusercontent.com/yt-dlp/yt-dlp/master/LICENSE' $ytDlpLicense '(?i)(public domain|unlicense)'
    Download-LicenseText 'https://raw.githubusercontent.com/denoland/deno/main/LICENSE' $denoLicense '(?i)MIT License'

    $ffmpegLicense = [IO.File]::ReadAllText((Join-Path $bin 'FFmpeg-LICENSE.txt'))
    if ($ffmpegLicense.Length -lt 1000 -or $ffmpegLicense -notmatch '(?i)FFmpeg') {
        throw 'The extracted FFmpeg license text failed validation.'
    }

    $noticesBuilder = [Text.StringBuilder]::new()
    [void]$noticesBuilder.AppendLine('YouTube Downloader Desktop Beta — Third-Party Notices')
    [void]$noticesBuilder.AppendLine('')
    [void]$noticesBuilder.AppendLine('This application is licensed under GNU AGPL version 3; see the complete license text below.')
    [void]$noticesBuilder.AppendLine('Bundled runtime components and their license texts follow.')
    [void]$noticesBuilder.AppendLine('')
    [void]$noticesBuilder.AppendLine('yt-dlp — https://github.com/yt-dlp/yt-dlp — license text follows.')
    [void]$noticesBuilder.AppendLine('')
    [void]$noticesBuilder.AppendLine([IO.File]::ReadAllText($ytDlpLicense).TrimEnd())
    [void]$noticesBuilder.AppendLine('')
    [void]$noticesBuilder.AppendLine('FFmpeg — https://ffmpeg.org/ — license text supplied by the verified FFmpeg essentials build follows.')
    [void]$noticesBuilder.AppendLine('')
    [void]$noticesBuilder.AppendLine($ffmpegLicense.TrimEnd())
    [void]$noticesBuilder.AppendLine('')
    [void]$noticesBuilder.AppendLine('Deno — https://deno.com/ — license text follows.')
    [void]$noticesBuilder.AppendLine('')
    [void]$noticesBuilder.AppendLine([IO.File]::ReadAllText($denoLicense).TrimEnd())
    [void]$noticesBuilder.AppendLine('')
    [void]$noticesBuilder.AppendLine('Slint UI attribution and license information are available in the application About & legal section and at https://slint.dev/.')
    [void]$noticesBuilder.AppendLine('')
    [void]$noticesBuilder.AppendLine('----- YouTube Downloader Desktop source license: GNU AGPL version 3 -----')
    [void]$noticesBuilder.AppendLine('')
    [void]$noticesBuilder.AppendLine([IO.File]::ReadAllText((Join-Path $root 'LICENSE')).TrimEnd())
    $stagedNotices = Join-Path $stage 'THIRD-PARTY-NOTICES.txt'
    [IO.File]::WriteAllText($stagedNotices, $noticesBuilder.ToString() + "`r`n", [Text.UTF8Encoding]::new($false))

    $payload = Join-Path $stage 'runtime-payload.bin'
    $payloadHash = New-EmbeddedRuntimePayload $verifiedBin $payload (Join-Path $stage 'compressed')
    if ($payloadHash -notmatch '^(?i:[0-9a-f]{64})$' -or
        (Get-FileHash -LiteralPath $payload -Algorithm SHA256).Hash.ToUpperInvariant() -ne $payloadHash) {
        throw 'Embedded payload SHA-256 verification failed.'
    }
    Write-Host "Verified embedded payload: $payloadHash"

    $cargoCommand = Get-Command cargo -ErrorAction SilentlyContinue
    $cargo = if ($cargoCommand) { $cargoCommand.Source } else { Join-Path $env:USERPROFILE '.cargo\bin\cargo.exe' }
    if (-not (Test-Path -LiteralPath $cargo -PathType Leaf)) { throw 'Rust Cargo toolchain unavailable.' }

    $environmentNames = @('YTD_EMBEDDED_RUNTIME_PAYLOAD', 'YTD_RUNTIME_PAYLOAD_SHA256', 'YTD_REQUIRE_EMBEDDED_RUNTIME')
    $previousEnvironment = @{}
    foreach ($name in $environmentNames) { $previousEnvironment[$name] = [Environment]::GetEnvironmentVariable($name, 'Process') }
    try {
        $env:YTD_EMBEDDED_RUNTIME_PAYLOAD = $payload
        $env:YTD_RUNTIME_PAYLOAD_SHA256 = $payloadHash
        $env:YTD_REQUIRE_EMBEDDED_RUNTIME = '1'
        Push-Location $root
        try {
            & $cargo build --release --locked
            if ($LASTEXITCODE -ne 0) { throw "Rust release build failed with exit code $LASTEXITCODE." }
        } finally { Pop-Location }
    } finally {
        foreach ($name in $environmentNames) {
            [Environment]::SetEnvironmentVariable($name, $previousEnvironment[$name], 'Process')
        }
    }

    $executable = Join-Path $root 'target\release\youtube-downloader-desktop.exe'
    Assert-WindowsExecutable $executable
    $stagedExe = Join-Path $stage ($outputName + '.exe')
    Copy-Item -LiteralPath $executable -Destination $stagedExe
    $builtHash = (Get-FileHash -LiteralPath $stagedExe -Algorithm SHA256).Hash.ToUpperInvariant()
    if ((Get-Item -LiteralPath $stagedExe).Length -lt 20MB) {
        throw 'Release executable is too small to contain the standalone runtime payload.'
    }

    # Exercise the actual EXE twice before publication, with no sibling bin
    # directory and an isolated LOCALAPPDATA. The runtime's own verifier must
    # confirm its embedded payload, and the extracted files must match the
    # checksum-verified inputs byte-for-byte after both runs.
    if (Test-Path -LiteralPath (Join-Path (Split-Path -Parent $stagedExe) 'bin')) {
        throw 'Standalone smoke test staging directory unexpectedly contains a sibling bin folder.'
    }
    $isolatedLocalAppData = Join-Path $stage 'isolated-localappdata'
    New-Item -ItemType Directory -Path $isolatedLocalAppData | Out-Null
    $previousLocalAppData = [Environment]::GetEnvironmentVariable('LOCALAPPDATA', 'Process')
    try {
        [Environment]::SetEnvironmentVariable('LOCALAPPDATA', $isolatedLocalAppData, 'Process')
        for ($run = 1; $run -le 2; $run++) {
            $verificationOutput = @(& $stagedExe --verify-embedded-runtime 2>&1)
            $verifyExitCode = $LASTEXITCODE
            if ($verifyExitCode -ne 0) {
                $preserveStageOnFailure = $true
                $details = ($verificationOutput | Out-String).Trim()
                throw "Standalone embedded-runtime verification run $run failed with exit code $verifyExitCode. $details"
            }
            Assert-ExtractedRuntimeMatches $isolatedLocalAppData $verifiedBin
            Write-Host "Embedded runtime verification run $run passed."
        }
    } catch {
        $preserveStageOnFailure = $true
        throw
    } finally {
        [Environment]::SetEnvironmentVariable('LOCALAPPDATA', $previousLocalAppData, 'Process')
    }

    # Move the notices first and the EXE last. The EXE is the final success
    # marker; if publication fails, remove only the notices created here.
    $noticesPublished = $false
    try {
        [IO.File]::Move($stagedNotices, $outputNotices)
        $noticesPublished = $true
        [IO.File]::Move($stagedExe, $outputExe)
    } catch {
        if ($noticesPublished -and (Test-Path -LiteralPath $outputNotices) -and
            -not (Test-Path -LiteralPath $outputExe)) {
            Remove-Item -LiteralPath $outputNotices -Force
        }
        throw
    }

    Write-Host "Standalone executable built: $outputExe"
    Write-Host "Third-party license notices: $outputNotices"
    Write-Host "Executable SHA-256: $builtHash"
    Write-Host 'The EXE embeds all four runtime tools. The notices file is accompanying license documentation.'
} finally {
    if (Test-Path -LiteralPath $stage) {
        if ($preserveStageOnFailure) {
            Write-Warning "Standalone verification failed; diagnostic staging files were retained at $stage"
        } else {
            Remove-Item -LiteralPath $stage -Recurse -Force
        }
    }
}

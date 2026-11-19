param(
    [string]$Executable = "",
    [ValidateSet("red","mono")][string]$Theme = "red",
    [ValidateSet("none","library","setup","queue")][string]$Preview = "none",
    [ValidateSet("normal","maximized","fullscreen","compact","minimum")][string]$WindowMode = "normal",
    [ValidateSet("none","channel","download","queue")][string]$ErrorPreview = "none",
    [switch]$EmptyState,
    [switch]$ProbeAccessibility,
    [switch]$ExerciseControls,
    [switch]$PhysicalPrimaryActions,
    [switch]$PhysicalNavigationOnly,
    [switch]$LongTitles,
    [switch]$ExerciseLicensedDownload,
    [switch]$FocusPreview,
    [switch]$ReuseSession,
    [switch]$ExerciseTitlePopup,
    [switch]$CaptureExpandedTitle,
    [switch]$ExerciseSelectionPopup,
    [switch]$ExerciseQueueHistory,
    [switch]$ExerciseRealChannelLoad,
    [switch]$ExerciseRealChannelPagination,
    [string]$RealChannelUrl = 'https://www.youtube.com/@YouTube/videos',
    [switch]$CaptureHistoryConfirmation,
    [switch]$ExerciseWindowToggle,
    [switch]$ActiveQueue,
    [switch]$VerifyRestoredSession,
    [switch]$VerifyEmptyQueue,
    [switch]$MouseFilteredClear,
    [ValidateSet('uia','mouse','keyboard')][string]$FilteredActionInput = 'uia',
    [string]$SessionRoot = "",
    [switch]$AllowStaleExecutable
)
$ErrorActionPreference = 'Stop'
$root = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$smokeRuns = [IO.Path]::GetFullPath((Join-Path $root 'dist\smoke-runs'))
if (-not $SessionRoot) {
    if ($ReuseSession) { throw 'ReuseSession requires the exact -SessionRoot printed by the first isolated run.' }
    $SessionRoot = Join-Path $smokeRuns ([guid]::NewGuid().ToString('N'))
}
$previewRoot = [IO.Path]::GetFullPath($SessionRoot)
if (-not $previewRoot.StartsWith(($smokeRuns + [IO.Path]::DirectorySeparatorChar), [StringComparison]::OrdinalIgnoreCase)) {
    throw 'SessionRoot must be a child of dist\smoke-runs; existing saved sessions are protected.'
}
if ($VerifyRestoredSession -and (-not $ReuseSession -or $Preview -ne 'library')) {
    throw 'VerifyRestoredSession requires -ReuseSession with Library preview.'
}
if ($ExerciseWindowToggle -and $WindowMode -ne 'normal') {
    throw 'ExerciseWindowToggle requires normal mode to check round-trip window geometry.'
}
if ($VerifyEmptyQueue -and ($Preview -ne 'queue' -or -not $EmptyState)) {
    throw 'VerifyEmptyQueue requires -Preview queue -EmptyState.'
}
if ($ReuseSession -and -not (Test-Path -LiteralPath (Join-Path $previewRoot 'YouTube Downloader Desktop\session.json'))) {
    throw 'ReuseSession requested, but the isolated session file does not exist.'
}
if ($ExerciseLicensedDownload -and (-not $ExerciseControls -or $EmptyState -or $Preview -ne 'library')) {
    throw 'ExerciseLicensedDownload requires -ExerciseControls and a populated Library preview.'
}
if ($PhysicalNavigationOnly -and (-not $PhysicalPrimaryActions -or -not $ExerciseControls)) {
    throw 'PhysicalNavigationOnly requires ExerciseControls and PhysicalPrimaryActions.'
}
if ($ReuseSession -and ($Preview -eq 'none' -or $ExerciseControls -or $EmptyState)) {
    throw 'ReuseSession requires a preview without interaction or empty-state reset.'
}
if ($ExerciseTitlePopup -and (-not $LongTitles -or $Preview -notin @('library','queue'))) {
    throw 'ExerciseTitlePopup requires -LongTitles and Library or Downloads preview.'
}
if ($CaptureExpandedTitle -and -not $ExerciseTitlePopup) {
    throw 'CaptureExpandedTitle requires ExerciseTitlePopup.'
}
if ($ExerciseSelectionPopup -and $Preview -ne 'setup') {
    throw 'ExerciseSelectionPopup requires a Download Options preview.'
}
if ($ExerciseQueueHistory -and ($Preview -ne 'queue' -or $EmptyState)) {
    throw 'ExerciseQueueHistory requires a populated Downloads preview.'
}
if ($CaptureHistoryConfirmation -and ($Preview -ne 'queue' -or $EmptyState -or $ExerciseQueueHistory)) {
    throw 'CaptureHistoryConfirmation requires a populated Downloads preview without ExerciseQueueHistory.'
}
if ($ActiveQueue -and ($Preview -ne 'queue' -or $EmptyState)) {
    throw 'ActiveQueue requires a populated Downloads preview.'
}
if ($ExerciseRealChannelLoad -and ($Preview -ne 'library' -or -not $EmptyState -or $ExerciseControls -or $ReuseSession)) {
    throw 'ExerciseRealChannelLoad requires isolated -Preview library -EmptyState without ExerciseControls or ReuseSession.'
}
if ($ExerciseRealChannelPagination -and (-not $ExerciseRealChannelLoad -or $WindowMode -ne 'normal')) {
    throw 'ExerciseRealChannelPagination requires ExerciseRealChannelLoad and a normal-size window.'
}
if (-not $Executable) {
    $packagedBeta = Join-Path $root 'dist\YouTube Downloader Desktop Beta\YouTube Downloader Desktop.exe'
    $Executable = if (Test-Path -LiteralPath $packagedBeta) { $packagedBeta } else {
        Join-Path $root 'target\release\youtube-downloader-desktop.exe'
    }
}
if (-not (Test-Path -LiteralPath $Executable)) { throw "Missing app: $Executable" }
$Executable = [IO.Path]::GetFullPath($Executable)
function Get-NewestBetaSourceTimestamp {
    $sourceFiles = @(
        Get-ChildItem -LiteralPath (Join-Path $root 'src') -Recurse -File -Filter '*.rs'
        Get-ChildItem -LiteralPath (Join-Path $root 'ui') -Recurse -File -Filter '*.slint'
        Get-Item -LiteralPath (Join-Path $root 'build.rs')
        Get-Item -LiteralPath (Join-Path $root 'Cargo.toml')
        Get-Item -LiteralPath (Join-Path $root 'Cargo.lock')
    )
    return @($sourceFiles | ForEach-Object { $_.LastWriteTimeUtc } | Sort-Object -Descending)[0]
}
$newestSource = Get-NewestBetaSourceTimestamp
$buildTimestamp = (Get-Item -LiteralPath $Executable).LastWriteTimeUtc
$buildHash = (Get-FileHash -LiteralPath $Executable -Algorithm SHA256).Hash
if ($buildTimestamp -lt $newestSource -and -not $AllowStaleExecutable) {
    throw ("Executable was built at {0:u}, before the latest source modification {1:u}. Build the current beta first (or use -AllowStaleExecutable to diagnose an old binary)." -f $buildTimestamp, $newestSource)
}
Write-Output ('GUI_EXECUTABLE=' + $Executable)
Write-Output ('GUI_EXECUTABLE_UTC=' + $buildTimestamp.ToString('o'))
Write-Output ('GUI_EXECUTABLE_SHA256=' + $buildHash)
Write-Output ('GUI_LATEST_SOURCE_UTC=' + $newestSource.ToString('o'))
if ($buildTimestamp -lt $newestSource) { Write-Output 'GUI_STALE_BINARY_WARNING=true' }
Write-Output ('GUI_ISOLATED_LOCALAPPDATA=' + $previewRoot)
$source = @'
using System;
using System.Runtime.InteropServices;
public static class WindowCapture {
    [StructLayout(LayoutKind.Sequential)]
    public struct Rect { public int Left, Top, Right, Bottom; }
    public delegate bool EnumCallback(IntPtr hwnd, IntPtr parameter);
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumCallback callback, IntPtr parameter);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr hwnd, out uint pid);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetWindowText(IntPtr hwnd, System.Text.StringBuilder title, int capacity);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr hWnd, out Rect rect);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr hWnd, IntPtr hdcBlt, uint nFlags);
    [DllImport("user32.dll")] public static extern bool IsZoomed(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern bool IsIconic(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr hWnd, int nCmdShow);
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
    [DllImport("user32.dll")] public static extern short GetAsyncKeyState(int vKey);
    [DllImport("user32.dll", EntryPoint="GetWindowLongW")] public static extern int GetWindowLong(IntPtr hWnd, int index);
    [StructLayout(LayoutKind.Sequential)] public struct Point { public int X, Y; }
    [DllImport("user32.dll")] public static extern bool GetCursorPos(out Point point);
    [DllImport("user32.dll")] public static extern bool SetCursorPos(int x, int y);
    [DllImport("user32.dll")] public static extern IntPtr WindowFromPoint(Point point);
    [DllImport("user32.dll")] public static extern void mouse_event(uint flags, uint dx, uint dy, uint data, UIntPtr extraInfo);
}
'@
if (-not ([System.Management.Automation.PSTypeName]'WindowCapture').Type) {
    Add-Type -TypeDefinition $source -ErrorAction Stop
}
Add-Type -AssemblyName System.Drawing
$workdir = Split-Path -Parent $Executable
$previousTheme = [Environment]::GetEnvironmentVariable("YTD_UI_SMOKE_THEME", "Process")
$previousPage = [Environment]::GetEnvironmentVariable("YTD_UI_SMOKE_PAGE", "Process")
$previousLocalAppData = [Environment]::GetEnvironmentVariable("LOCALAPPDATA", "Process")
$previousWindowMode = [Environment]::GetEnvironmentVariable("YTD_UI_SMOKE_WINDOW_MODE", "Process")
$previousSmokePreview = [Environment]::GetEnvironmentVariable("YTD_UI_SMOKE_PREVIEW", "Process")
$previousErrorPreview = [Environment]::GetEnvironmentVariable("YTD_UI_SMOKE_ERROR", "Process")
$previousActivePreview = [Environment]::GetEnvironmentVariable("YTD_UI_SMOKE_ACTIVE", "Process")
if ($Preview -ne "none" -and -not $ReuseSession) {
    $sessionDir = Join-Path $previewRoot 'YouTube Downloader Desktop'
    New-Item -ItemType Directory -Force -Path $sessionDir | Out-Null
    $videos = @(
        @{ id = 'dQw4w9WgXcQ'; title = 'Rick Astley - Never Gonna Give You Up'; duration = '3:33' },
        @{ id = 'jNQXAC9IVRw'; title = 'Me at the zoo - original YouTube upload'; duration = '0:19' },
        @{ id = 'aqz-KE-bpKQ'; title = 'Big Buck Bunny - animation short film'; duration = '9:56' },
        @{ id = '9bZkp7q19f0'; title = 'PSY - Gangnam Style'; duration = '4:13' },
        @{ id = 'LXb3EKWsInQ'; title = 'Costa Rica in 4K - wildlife and scenery'; duration = '5:14' }
    )
    if ($LongTitles) {
        $videos[0].title = 'A very long creator gameplay walkthrough with unusually detailed chapter commentary and an extended descriptive title that should truncate without breaking controls'
        $videos[1].title = 'Another extended video title exploring download options, accessibility and responsiveness at every available desktop window size'
    }
    $thumbnailDir = Join-Path $sessionDir 'thumbnails'
    New-Item -ItemType Directory -Force -Path $thumbnailDir | Out-Null
    foreach ($video in $(if($EmptyState){@()}else{$videos})) {
        $imagePath = Join-Path $thumbnailDir ($video.id + '.jpg')
        if (-not (Test-Path -LiteralPath $imagePath)) {
            try {
                $fixtureThumbnail = Join-Path $root ('dist\smoke-data\YouTube Downloader Desktop\thumbnails\' + $video.id + '.jpg')
                if (Test-Path -LiteralPath $fixtureThumbnail) {
                    Copy-Item -LiteralPath $fixtureThumbnail -Destination $imagePath
                } else {
                    $client = New-Object Net.WebClient
                    try { $client.DownloadFile(('https://i.ytimg.com/vi/'+$video.id+'/mqdefault.jpg'), $imagePath) }
                    finally { $client.Dispose() }
                }
            } catch {
                Write-Output ('THUMBNAIL_UNAVAILABLE: '+$video.id+' '+$_.Exception.Message)
            }
        }
    }
    $library = @()
    for ($i=0; $i -lt $videos.Count; $i++) {
        $library += @{ video = $videos[$i]; selected = ($i -lt 3) }
    }
    $jobs = @(
        @{ video = $videos[0]; mode = 'video'; quality = 'best'; folder = 'C:\Downloads'; state = 'completed'; detail = 'Saved successfully' },
        @{ video = $videos[1]; mode = 'audio'; quality = '720'; folder = 'C:\Downloads'; state = 'failed'; detail = 'Connection failed; retry when ready' },
        @{ video = $videos[2]; mode = 'captions'; quality = 'best'; folder = 'C:\Downloads'; state = 'completed'; detail = 'Saved captions' }
    )
    if ($EmptyState) {
        $library = @()
        $jobs = @()
    }
    $snapshot = @{
        schema = 1; theme = $Theme; channel_name = $(if($EmptyState){''}else{'Example Creator'});
        channel_url = $(if($EmptyState){''}else{'https://www.youtube.com/@ExampleCreator/videos'});
        folder = 'C:\Downloads'; videos = $library; jobs = $jobs
    }
    if ($ExerciseQueueHistory) {
        $unavailableFolder = Join-Path $previewRoot 'missing-queue-folder'
        foreach ($job in $snapshot.jobs) { $job.folder = $unavailableFolder }
    }
    if ($ExerciseLicensedDownload) {
        $functionalOutput = Join-Path $previewRoot 'licensed-download-output'
        New-Item -ItemType Directory -Force -Path $functionalOutput | Out-Null
        $snapshot.folder = $functionalOutput
        Write-Output ('GUI_LICENSED_DOWNLOAD_OUTPUT=' + $functionalOutput)
    }
    $json = $snapshot | ConvertTo-Json -Depth 10 -Compress
    [IO.File]::WriteAllText((Join-Path $sessionDir 'session.json'), $json, (New-Object Text.UTF8Encoding($false)))
} elseif ($Preview -ne "none") {
    $existingSession = Join-Path $previewRoot 'YouTube Downloader Desktop\session.json'
    if (-not (Test-Path -LiteralPath $existingSession)) { throw 'No isolated preview session exists to restore.' }
}
try {
    [Environment]::SetEnvironmentVariable("YTD_UI_SMOKE_THEME", $(if($ReuseSession){$null}else{$Theme}), "Process")
    [Environment]::SetEnvironmentVariable("YTD_UI_SMOKE_PAGE", $(if($Preview -eq 'queue'){'queue'}elseif($Preview -eq 'setup'){'setup'}else{'discover'}), "Process")
    [Environment]::SetEnvironmentVariable("YTD_UI_SMOKE_WINDOW_MODE", $WindowMode, "Process")
    [Environment]::SetEnvironmentVariable("YTD_UI_SMOKE_ERROR", $ErrorPreview, "Process")
    [Environment]::SetEnvironmentVariable("YTD_UI_SMOKE_ACTIVE", $(if($ActiveQueue){"1"}else{$null}), "Process")
    [Environment]::SetEnvironmentVariable("YTD_UI_SMOKE_PREVIEW", "1", "Process")
    [Environment]::SetEnvironmentVariable("LOCALAPPDATA", $previewRoot, "Process")
    $process = Start-Process -FilePath $Executable -WorkingDirectory $workdir -PassThru
} finally {
    [Environment]::SetEnvironmentVariable("YTD_UI_SMOKE_THEME", $previousTheme, "Process")
    [Environment]::SetEnvironmentVariable("YTD_UI_SMOKE_PAGE", $previousPage, "Process")
    [Environment]::SetEnvironmentVariable("LOCALAPPDATA", $previousLocalAppData, "Process")
    [Environment]::SetEnvironmentVariable("YTD_UI_SMOKE_WINDOW_MODE", $previousWindowMode, "Process")
    [Environment]::SetEnvironmentVariable("YTD_UI_SMOKE_PREVIEW", $previousSmokePreview, "Process")
    [Environment]::SetEnvironmentVariable("YTD_UI_SMOKE_ERROR", $previousErrorPreview, "Process")
    [Environment]::SetEnvironmentVariable("YTD_UI_SMOKE_ACTIVE", $previousActivePreview, "Process")
}
try {
    $handle = [IntPtr]::Zero
    for ($attempt = 0; $attempt -lt 40; $attempt++) {
        Start-Sleep -Milliseconds 200
        $live = Get-Process -Id $process.Id -ErrorAction SilentlyContinue
        if (-not $live) { throw 'GUI process exited unexpectedly.' }
        # Slint/winit creates a separate visible 16x16 event-target window
        # which Process.MainWindowHandle can mistake for the application.
        $candidate = [pscustomobject]@{ Handle = [IntPtr]::Zero }
        $callback = [WindowCapture+EnumCallback]{
            param($hwnd, $unused)
            [uint32]$ownerPid = 0
            [void][WindowCapture]::GetWindowThreadProcessId($hwnd, [ref]$ownerPid)
            if ($ownerPid -eq $process.Id -and [WindowCapture]::IsWindowVisible($hwnd)) {
                $caption = New-Object System.Text.StringBuilder(256)
                [void][WindowCapture]::GetWindowText($hwnd, $caption, 256)
                if ($caption.ToString() -like 'YouTube Downloader*Desktop*') {
                    $candidate.Handle = $hwnd
                }
            }
            return $true
        }
        [void][WindowCapture]::EnumWindows($callback, [IntPtr]::Zero)
        $handle = $candidate.Handle
        if ($handle -ne [IntPtr]::Zero) { break }
    }
    if ($handle -eq [IntPtr]::Zero) { throw 'No visible native main window found.' }
    if (-not [WindowCapture]::IsWindowVisible($handle)) { throw 'Main window is not visible.' }
    $titleBuffer = New-Object System.Text.StringBuilder(256)
    [void][WindowCapture]::GetWindowText($handle, $titleBuffer, $titleBuffer.Capacity)
    $windowTitle = $titleBuffer.ToString()
    $expectedTitle = 'YouTube Downloader Desktop ' + [char]0x2014 + ' Beta'
    if ($windowTitle -ne $expectedTitle) {
        if (-not $AllowStaleExecutable) {
            throw ('Unexpected application title; this may be an older build: ' + $windowTitle)
        }
        Write-Output 'GUI_STALE_TITLE_WARNING=true'
    }
    Write-Output ('GUI_WINDOW_TITLE=' + $windowTitle)
    $rect = New-Object WindowCapture+Rect
    $width = 0
    $height = 0
    for ($attempt = 0; $attempt -lt 40; $attempt++) {
        if (-not [WindowCapture]::GetWindowRect($handle, [ref]$rect)) { throw 'Unable to measure main window geometry.' }
        $width = $rect.Right - $rect.Left
        $height = $rect.Bottom - $rect.Top
        if ($width -ge 650 -and $height -ge 400) { break }
        Start-Sleep -Milliseconds 200
    }
    if ($width -lt 650 -or $height -lt 400) { throw ('Window too small: ' + $width + 'x' + $height) }
    if ($ExerciseControls) {
        if ($Preview -ne 'library' -or $EmptyState) {
            throw 'ExerciseControls requires a populated library preview.'
        }
        Add-Type -AssemblyName UIAutomationClient
        Add-Type -AssemblyName UIAutomationTypes
        $automationRoot = [System.Windows.Automation.AutomationElement]::FromHandle($handle)
        function Find-NativeControl([string]$name, $controlType) {
            # Slint's UIA tree changes when navigating; a provider-side
            # ControlType filter can return a stale subset after a page switch.
            $matches = $automationRoot.FindAll(
                [System.Windows.Automation.TreeScope]::Descendants,
                [System.Windows.Automation.Condition]::TrueCondition
            )
            foreach ($candidate in $matches) {
                if ($candidate.Current.ControlType.ProgrammaticName -eq $controlType.ProgrammaticName -and $candidate.Current.Name -like $name) {
                    return $candidate
                }
            }
            return $null
        }
        function Invoke-NativeButton([string]$name, [switch]$UseRealMouse, [switch]$UseKeyboard) {
            if ([WindowCapture]::IsIconic($handle)) {
                [void][WindowCapture]::ShowWindow($handle, 9)
                Start-Sleep -Milliseconds 350
                Write-Output 'GUI_WINDOW_RESTORED_FROM_MINIMIZED'
            }
            $button = Find-NativeControl $name ([System.Windows.Automation.ControlType]::Button)
            if (-not $button) { throw "Missing accessible button: $name" }
            $bounds = $button.Current.BoundingRectangle
            Write-Output ('GUI_INVOKE=' + $name + ' bounds=' + [Math]::Round($bounds.X) + ',' + [Math]::Round($bounds.Y) + ',' + [Math]::Round($bounds.Width) + ',' + [Math]::Round($bounds.Height))
            if ($UseKeyboard) {
                Add-Type -AssemblyName System.Windows.Forms
                $previousForeground = [WindowCapture]::GetForegroundWindow()
                try {
                    Test-WindowDimensions ('KEYBOARD_' + $name + '_BEFORE_FOREGROUND')
                    [void][WindowCapture]::SetForegroundWindow($handle)
                    Start-Sleep -Milliseconds 150
                    Test-WindowDimensions ('KEYBOARD_' + $name + '_AFTER_FOREGROUND')
                    $button.SetFocus()
                    Start-Sleep -Milliseconds 150
                    Test-WindowDimensions ('KEYBOARD_' + $name + '_AFTER_FOCUS')
                    $focused = [System.Windows.Automation.AutomationElement]::FocusedElement
                    $focusName = $focused.Current.Name
                    $focusPid = $focused.Current.ProcessId
                    Write-Output ('GUI_KEYBOARD_TARGET_FOCUS name=' + $focusName + ' pid=' + $focusPid)
                    if ($focusPid -ne $process.Id -or ($focusName -ne $name -and -not [string]::IsNullOrEmpty($focusName))) {
                        throw ('Keyboard target did not receive focus: ' + $focusName + ' process=' + $focusPid)
                    }
                    $modifierNames = @{ 16='SHIFT';17='CTRL';18='ALT';91='LWIN';92='RWIN' }
                    $heldModifiers = @($modifierNames.Keys | Where-Object {
                        (([int][WindowCapture]::GetAsyncKeyState([int]$_)) -band 0x8000) -ne 0
                    } | ForEach-Object { $modifierNames[$_] })
                    Write-Output ('GUI_KEYBOARD_MODIFIERS_BEFORE=' + $(if($heldModifiers.Count){$heldModifiers -join ','}else{'none'}))
                    if ($heldModifiers.Count) { throw ('Skipping Space because modifier keys are held: ' + ($heldModifiers -join ',')) }
                    [uint32]$beforeSendPid = 0
                    [void][WindowCapture]::GetWindowThreadProcessId([WindowCapture]::GetForegroundWindow(), [ref]$beforeSendPid)
                    Write-Output ('GUI_KEYBOARD_FOREGROUND_PID=' + $beforeSendPid)
                    if ($beforeSendPid -ne $process.Id) { throw 'Refusing keyboard input because test app lost foreground.' }
                    [System.Windows.Forms.SendKeys]::SendWait(' ')
                    Test-WindowDimensions ('KEYBOARD_' + $name + '_AFTER_SPACE')
                    Write-Output ('GUI_KEYBOARD_INVOKE=' + $name)
                } finally {
                    if ($previousForeground -ne [IntPtr]::Zero) {
                        [void][WindowCapture]::SetForegroundWindow($previousForeground)
                    }
                    Test-WindowDimensions ('KEYBOARD_' + $name + '_AFTER_RESTORE_FOREGROUND')
                }
            } elseif ($UseRealMouse) {
                $targetX = [int][Math]::Round($bounds.X + $bounds.Width / 2)
                $targetY = [int][Math]::Round($bounds.Y + $bounds.Height / 2)
                $nativeBounds = New-Object WindowCapture+Rect
                if (-not [WindowCapture]::GetWindowRect($handle, [ref]$nativeBounds) -or
                    $bounds.Width -lt 4 -or $bounds.Height -lt 4 -or
                    $targetX -le $nativeBounds.Left -or $targetX -ge $nativeBounds.Right -or
                    $targetY -le $nativeBounds.Top -or $targetY -ge $nativeBounds.Bottom) {
                    throw ('Refusing physical click outside isolated application: ' + $name)
                }
                $oldCursor = New-Object WindowCapture+Point
                if (-not [WindowCapture]::GetCursorPos([ref]$oldCursor)) { throw 'Could not preserve cursor position.' }
                $previousForeground = [WindowCapture]::GetForegroundWindow()
                try {
                    # Windows sometimes refuses foreground activation from
                    # an automation process. Focus an UNRELATED control only
                    # to activate the test window; never focus the click
                    # target, because that can mask pointer hit-test defects.
                    $activationName = if ($name -eq 'Mono') { 'Library' } else { 'Mono' }
                    $activationControl = Find-NativeControl $activationName ([System.Windows.Automation.ControlType]::Button)
                    if (-not $activationControl) { throw 'No safe foreground activation control for mouse test.' }
                    $activationControl.SetFocus()
                    Start-Sleep -Milliseconds 140
                    [void][WindowCapture]::SetForegroundWindow($handle)
                    Start-Sleep -Milliseconds 180
                    $foreground = [WindowCapture]::GetForegroundWindow()
                    [uint32]$foregroundPid = 0
                    [void][WindowCapture]::GetWindowThreadProcessId($foreground, [ref]$foregroundPid)
                    if ($foregroundPid -ne $process.Id) {
                        # Keyboard tests can return foreground ownership to
                        # the test runner. Simulate a user activating the
                        # application's native title bar, then retry; never
                        # send a functional click to a foreign foreground.
                        $captionX = [int]($nativeBounds.Left + 145)
                        $captionY = [int]($nativeBounds.Top + 18)
                        [void][WindowCapture]::SetCursorPos($captionX, $captionY)
                        [WindowCapture]::mouse_event(0x0002, 0, 0, 0, [UIntPtr]::Zero)
                        Start-Sleep -Milliseconds 75
                        [WindowCapture]::mouse_event(0x0004, 0, 0, 0, [UIntPtr]::Zero)
                        Start-Sleep -Milliseconds 220
                        [void][WindowCapture]::SetForegroundWindow($handle)
                        Start-Sleep -Milliseconds 130
                        $foreground = [WindowCapture]::GetForegroundWindow()
                        [void][WindowCapture]::GetWindowThreadProcessId($foreground, [ref]$foregroundPid)
                        Write-Output ('GUI_FOREGROUND_ACTIVATION_RETRY=' + $foregroundPid)
                    }
                    if ($foregroundPid -ne $process.Id) { throw 'Refusing physical click because test app does not own foreground.' }
                    if (-not [WindowCapture]::SetCursorPos($targetX, $targetY)) { throw 'Could not position mouse over test button.' }
                    Start-Sleep -Milliseconds 100
                    $hitPosition = New-Object WindowCapture+Point
                    [void][WindowCapture]::GetCursorPos([ref]$hitPosition)
                    $hitHandle = [WindowCapture]::WindowFromPoint($hitPosition)
                    [uint32]$hitPid = 0
                    [void][WindowCapture]::GetWindowThreadProcessId($hitHandle, [ref]$hitPid)
                    $foregroundDown = [WindowCapture]::GetForegroundWindow()
                    [uint32]$foregroundDownPid = 0
                    [void][WindowCapture]::GetWindowThreadProcessId($foregroundDown, [ref]$foregroundDownPid)
                    Write-Output ('GUI_MOUSE_HIT_TARGET=' + $name + ' expected=' + $targetX + ',' + $targetY + ' actual=' + $hitPosition.X + ',' + $hitPosition.Y + ' hitPid=' + $hitPid + ' hitHwnd=' + $hitHandle + ' appHwnd=' + $handle + ' foregroundDownPid=' + $foregroundDownPid + ' foregroundDownHwnd=' + $foregroundDown)
                    if ([Math]::Abs($hitPosition.X - $targetX) -gt 2 -or [Math]::Abs($hitPosition.Y - $targetY) -gt 2) {
                        throw ('Physical mouse test interrupted: cursor moved away from target ' + $name + '. This is not an app hit-test failure.')
                    }
                    if ($hitPid -ne $process.Id -or $foregroundDownPid -ne $process.Id) {
                        throw ('Refusing physical click: another process intercepts target or foreground: ' + $name)
                    }
                    [WindowCapture]::mouse_event(0x0002, 0, 0, 0, [UIntPtr]::Zero)
                    Start-Sleep -Milliseconds 100
                    [WindowCapture]::mouse_event(0x0004, 0, 0, 0, [UIntPtr]::Zero)
                    $afterClickCursor = New-Object WindowCapture+Point
                    [void][WindowCapture]::GetCursorPos([ref]$afterClickCursor)
                    Write-Output ('GUI_MOUSE_CURSOR_AFTER_UP=' + $afterClickCursor.X + ',' + $afterClickCursor.Y)
                    if ([Math]::Abs($afterClickCursor.X - $targetX) -gt 2 -or [Math]::Abs($afterClickCursor.Y - $targetY) -gt 2) {
                        throw ('Physical mouse test interrupted: cursor moved during click ' + $name + '. This is not an app hit-test failure.')
                    }
                    # Give the Slint event loop time to process mouse-up at
                    # the button before restoring the user's cursor.
                    Start-Sleep -Milliseconds 300
                    Write-Output ('GUI_REAL_MOUSE_CLICK=' + $name + ' x=' + $targetX + ' y=' + $targetY + ' method=mouse_event')
                } finally {
                    [void][WindowCapture]::SetCursorPos($oldCursor.X, $oldCursor.Y)
                    if ($previousForeground -ne [IntPtr]::Zero) {
                        [void][WindowCapture]::SetForegroundWindow($previousForeground)
                    }
                }
            } else {
                $invoke = $button.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern)
                $invoke.Invoke()
            }
            Start-Sleep -Milliseconds 300
        }
        function Test-WindowDimensions([string]$stage) {
            $now = New-Object WindowCapture+Rect
            [void][WindowCapture]::GetWindowRect($handle, [ref]$now)
            Write-Output ('GUI_SIZE_' + $stage + '=' + ($now.Right - $now.Left) + 'x' + ($now.Bottom - $now.Top) + ' maximized=' + [WindowCapture]::IsZoomed($handle) + ' minimized=' + [WindowCapture]::IsIconic($handle))
            if ([WindowCapture]::IsIconic($handle)) {
                [void][WindowCapture]::ShowWindow($handle, 9)
                Start-Sleep -Milliseconds 350
                Write-Output 'GUI_WINDOW_RESTORED_FROM_MINIMIZED'
            }
        }
        function Save-MouseDiagnostic([string]$stage) {
            $native = New-Object WindowCapture+Rect
            [void][WindowCapture]::GetWindowRect($handle, [ref]$native)
            $w = $native.Right - $native.Left
            $h = $native.Bottom - $native.Top
            $bmp = New-Object Drawing.Bitmap($w, $h)
            try {
                $g = [Drawing.Graphics]::FromImage($bmp)
                try {
                    $dc = $g.GetHdc()
                    try { [void][WindowCapture]::PrintWindow($handle, $dc, 2) }
                    finally { $g.ReleaseHdc($dc) }
                } finally { $g.Dispose() }
                $path = Join-Path $previewRoot ('mouse-diagnostic-' + $stage + '.png')
                $bmp.Save($path, [Drawing.Imaging.ImageFormat]::Png)
                Write-Output ('GUI_MOUSE_DIAGNOSTIC=' + $path)
            } finally { $bmp.Dispose() }
        }
        Test-WindowDimensions 'BEFORE_INTERACTION'
        $sessionFile = Join-Path $previewRoot 'YouTube Downloader Desktop\session.json'
        if ($PhysicalPrimaryActions) {
            Invoke-NativeButton 'Downloads' -UseRealMouse
            $queueAfterMouse = $automationRoot.FindAll([System.Windows.Automation.TreeScope]::Descendants, [System.Windows.Automation.Condition]::TrueCondition)
            if (@($queueAfterMouse | ForEach-Object { $_.Current.Name }) -notcontains 'Retry failed') {
                Save-MouseDiagnostic 'downloads'
                throw 'Mouse regression: Downloads tab did not navigate.'
            }
            Write-Output 'GUI_MOUSE_DOWNLOADS_TAB_PASS'
            Invoke-NativeButton 'Download options' -UseRealMouse
            $setupAfterMouse = $automationRoot.FindAll([System.Windows.Automation.TreeScope]::Descendants, [System.Windows.Automation.Condition]::TrueCondition)
            if (@($setupAfterMouse | ForEach-Object { $_.Current.Name }) -notcontains 'Output preferences') {
                Save-MouseDiagnostic 'options'
                throw 'Mouse regression: Download options tab did not navigate.'
            }
            Write-Output 'GUI_MOUSE_OPTIONS_TAB_PASS'
            Invoke-NativeButton 'Library' -UseRealMouse
            $libraryAfterMouse = $automationRoot.FindAll([System.Windows.Automation.TreeScope]::Descendants, [System.Windows.Automation.Condition]::TrueCondition)
            if (@($libraryAfterMouse | ForEach-Object { $_.Current.Name }) -notcontains 'Select visible') {
                throw 'Mouse regression: Library tab did not navigate.'
            }
            Write-Output 'GUI_MOUSE_LIBRARY_TAB_PASS'
            if ($PhysicalNavigationOnly) {
                Write-Output 'GUI_MOUSE_NAVIGATION_ONLY_PASS'
                return
            }
        }
        Invoke-NativeButton 'Mono' -UseRealMouse:$PhysicalPrimaryActions
        $afterTheme = Get-Content -LiteralPath $sessionFile -Raw | ConvertFrom-Json
        if ($afterTheme.theme -ne 'mono') { throw 'GUI interaction: theme change did not persist.' }
        Write-Output 'GUI_INTERACTION_THEME_PASS'
        Test-WindowDimensions 'AFTER_THEME'

        $videoName = 'Rick Astley - Never Gonna Give You Up'
        $checkbox = Find-NativeControl $videoName ([System.Windows.Automation.ControlType]::CheckBox)
        if (-not $checkbox) { throw 'Missing accessible video checkbox.' }
        $toggle = $checkbox.GetCurrentPattern([System.Windows.Automation.TogglePattern]::Pattern)
        $toggle.Toggle()
        Start-Sleep -Milliseconds 300
        $afterToggle = Get-Content -LiteralPath $sessionFile -Raw | ConvertFrom-Json
        if ($afterToggle.videos[0].selected -ne $false -or $afterToggle.videos[1].selected -ne $true) {
            throw 'GUI interaction: selected video toggle failed to persist correctly.'
        }
        Write-Output 'GUI_INTERACTION_SELECTION_PASS'
        Test-WindowDimensions 'AFTER_SELECTION'

        $editFields = @($automationRoot.FindAll(
            [System.Windows.Automation.TreeScope]::Descendants,
            [System.Windows.Automation.Condition]::TrueCondition
        ) | Where-Object { $_.Current.ControlType.ProgrammaticName -eq 'ControlType.Edit' })
        if ($editFields.Count -lt 2) { throw 'Missing accessible Library search field.' }
        $searchPattern = $editFields[1].GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern)
        $searchPattern.SetValue('no-such-video-title')
        $searchEmpty = $false
        for ($attempt=0; $attempt -lt 16; $attempt++) {
            $searchControls = $automationRoot.FindAll([System.Windows.Automation.TreeScope]::Descendants, [System.Windows.Automation.Condition]::TrueCondition)
            if (@($searchControls | ForEach-Object { $_.Current.Name }) -contains 'No matching videos') {
                $searchEmpty = $true
                break
            }
            Start-Sleep -Milliseconds 150
        }
        if (-not $searchEmpty) { throw 'GUI interaction: filtering to zero matches did not show the empty state.' }
        $searchPattern.SetValue('')
        Start-Sleep -Milliseconds 250
        if (-not (Find-NativeControl $videoName ([System.Windows.Automation.ControlType]::CheckBox))) {
            throw 'GUI interaction: clearing the search did not restore videos.'
        }
        Write-Output 'GUI_INTERACTION_SEARCH_PASS'
        Test-WindowDimensions 'AFTER_SEARCH'

        # Bulk actions must respect the active filter and leave hidden rows untouched.
        $searchPattern.SetValue('zoo')
        Start-Sleep -Milliseconds 250
        Test-WindowDimensions 'AFTER_FILTER_TYPED'
        Invoke-NativeButton 'Clear visible' -UseRealMouse:($MouseFilteredClear -or $FilteredActionInput -eq 'mouse') -UseKeyboard:($FilteredActionInput -eq 'keyboard')
        Test-WindowDimensions 'AFTER_FILTERED_CLEAR'
        $afterFilteredClear = Get-Content -LiteralPath $sessionFile -Raw | ConvertFrom-Json
        if ($afterFilteredClear.videos[1].selected -ne $false -or
            $afterFilteredClear.videos[2].selected -ne $true -or
            $afterFilteredClear.videos[0].selected -ne $false) {
            throw 'GUI interaction: filtered Clear visible changed hidden selected videos.'
        }
        Invoke-NativeButton 'Select visible' -UseRealMouse:($FilteredActionInput -eq 'mouse') -UseKeyboard:($FilteredActionInput -eq 'keyboard')
        Test-WindowDimensions 'AFTER_FILTERED_SELECT'
        $afterFilteredSelect = Get-Content -LiteralPath $sessionFile -Raw | ConvertFrom-Json
        if ($afterFilteredSelect.videos[1].selected -ne $true -or
            $afterFilteredSelect.videos[2].selected -ne $true -or
            $afterFilteredSelect.videos[0].selected -ne $false) {
            throw 'GUI interaction: filtered Select visible changed hidden selected videos.'
        }
        $searchPattern.SetValue('')
        Start-Sleep -Milliseconds 250
        Write-Output 'GUI_INTERACTION_FILTERED_SELECTION_PRESERVES_HIDDEN_PASS'
        Test-WindowDimensions 'AFTER_FILTER_RESET'

        Invoke-NativeButton 'Continue*' -UseRealMouse:$PhysicalPrimaryActions
        $settingsVisible = $false
        for ($attempt=0; $attempt -lt 18; $attempt++) {
            $controlsAfterContinue = $automationRoot.FindAll([System.Windows.Automation.TreeScope]::Descendants, [System.Windows.Automation.Condition]::TrueCondition)
            $namesAfterContinue = @($controlsAfterContinue | ForEach-Object { $_.Current.Name })
            if ($namesAfterContinue -contains 'Output preferences') { $settingsVisible = $true; break }
            Start-Sleep -Milliseconds 150
        }
        if (-not $settingsVisible) {
            throw 'GUI interaction: Continue did not navigate to download settings.'
        }
        Write-Output 'GUI_INTERACTION_CONTINUE_PASS'
        Test-WindowDimensions 'AFTER_CONTINUE'
        Invoke-NativeButton 'Downloads' -UseRealMouse:$PhysicalPrimaryActions
        $downloadsVisible = $false
        for ($attempt=0; $attempt -lt 18; $attempt++) {
            $controlsAfterDownloads = $automationRoot.FindAll([System.Windows.Automation.TreeScope]::Descendants, [System.Windows.Automation.Condition]::TrueCondition)
            $namesAfterDownloads = @($controlsAfterDownloads | ForEach-Object { $_.Current.Name })
            if ($namesAfterDownloads -contains 'Retry failed') { $downloadsVisible = $true; break }
            Start-Sleep -Milliseconds 150
        }
        if (-not $downloadsVisible) {
            throw 'GUI interaction: Downloads tab did not show the queue.'
        }
        Write-Output 'GUI_INTERACTION_DOWNLOADS_TAB_PASS'
        Test-WindowDimensions 'AFTER_DOWNLOADS_TAB'
        Invoke-NativeButton 'Library' -UseRealMouse:$PhysicalPrimaryActions
        Invoke-NativeButton 'Select visible'
        $selectedAll = Get-Content -LiteralPath $sessionFile -Raw | ConvertFrom-Json
        if (@($selectedAll.videos | Where-Object { -not $_.selected }).Count -ne 0) {
            throw 'GUI interaction: Select visible failed.'
        }
        Write-Output 'GUI_INTERACTION_SELECT_VISIBLE_PASS'
        Test-WindowDimensions 'AFTER_SELECT_ALL'
        Invoke-NativeButton 'Clear visible'
        $clearedAll = Get-Content -LiteralPath $sessionFile -Raw | ConvertFrom-Json
        if (@($clearedAll.videos | Where-Object { $_.selected }).Count -ne 0) {
            throw 'GUI interaction: Clear visible failed.'
        }
        Write-Output 'GUI_INTERACTION_CLEAR_VISIBLE_PASS'
        Test-WindowDimensions 'AFTER_CLEAR_ALL'
        $beforeKeyboardRect = New-Object WindowCapture+Rect
        [void][WindowCapture]::GetWindowRect($handle, [ref]$beforeKeyboardRect)
        Write-Output ('GUI_GEOMETRY_BEFORE_KEYBOARD=' + ($beforeKeyboardRect.Right - $beforeKeyboardRect.Left) + 'x' + ($beforeKeyboardRect.Bottom - $beforeKeyboardRect.Top))
        if ($ExerciseLicensedDownload) {
            Write-Output 'GUI_KEYBOARD_CHECK_SEPARATE_FROM_LICENSED_DOWNLOAD'
        } else {
        $originalForeground = [WindowCapture]::GetForegroundWindow()
        try {
            if ([WindowCapture]::SetForegroundWindow($handle)) {
                Add-Type -AssemblyName System.Windows.Forms
                $monoControl = Find-NativeControl 'Mono' ([System.Windows.Automation.ControlType]::Button)
                if (-not $monoControl) { throw 'Missing Mono button for keyboard test.' }
                $monoControl.SetFocus()
                Start-Sleep -Milliseconds 200
                $focusBefore = [System.Windows.Automation.AutomationElement]::FocusedElement.Current.Name
                [System.Windows.Forms.SendKeys]::SendWait('{TAB}')
                Start-Sleep -Milliseconds 250
                $focusAfter = [System.Windows.Automation.AutomationElement]::FocusedElement.Current.Name
                Write-Output ('GUI_TAB_FOCUS=' + $focusBefore + ' -> ' + $focusAfter)
                if ($focusBefore -eq 'Mono' -and $focusAfter -ne 'Mono' -and -not [string]::IsNullOrWhiteSpace($focusAfter)) {
                    Write-Output 'GUI_KEYBOARD_TAB_PASS'
                } else {
                    Write-Output 'GUI_KEYBOARD_TAB_UNVERIFIED'
                }
                $firstVideo = Find-NativeControl $videoName ([System.Windows.Automation.ControlType]::CheckBox)
                if (-not $firstVideo) { throw 'Missing first video checkbox for keyboard test.' }
                $firstVideo.SetFocus()
                Start-Sleep -Milliseconds 150
                [System.Windows.Forms.SendKeys]::SendWait(' ')
                Start-Sleep -Milliseconds 300
                $afterSpace = Get-Content -LiteralPath $sessionFile -Raw | ConvertFrom-Json
                $focusAfterFirstSpace = [System.Windows.Automation.AutomationElement]::FocusedElement
                Write-Output ('GUI_FOCUS_AFTER_FIRST_SPACE=' + $focusAfterFirstSpace.Current.Name + ' pid=' + $focusAfterFirstSpace.Current.ProcessId)
                if ($afterSpace.videos[0].selected -eq $true) {
                    Write-Output 'GUI_KEYBOARD_SPACE_PASS'
                    [System.Windows.Forms.SendKeys]::SendWait(' ')
                    Start-Sleep -Milliseconds 300
                    $restoredSpace = Get-Content -LiteralPath $sessionFile -Raw | ConvertFrom-Json
                    $focusAfterSecondSpace = [System.Windows.Automation.AutomationElement]::FocusedElement
                    Write-Output ('GUI_FOCUS_AFTER_SECOND_SPACE=' + $focusAfterSecondSpace.Current.Name + ' pid=' + $focusAfterSecondSpace.Current.ProcessId)
                    if ($restoredSpace.videos[0].selected -ne $false) {
                        throw 'GUI keyboard: Space did not toggle the checkbox back.'
                    }
                } else {
                    Write-Output 'GUI_KEYBOARD_SPACE_UNVERIFIED'
                }
            } else {
                Write-Output 'GUI_KEYBOARD_TAB_UNVERIFIED: foreground control unavailable'
            }
        } finally {
            if ($originalForeground -ne [IntPtr]::Zero) {
                [void][WindowCapture]::SetForegroundWindow($originalForeground)
            }
        }
        }
        $afterKeyboardRect = New-Object WindowCapture+Rect
        [void][WindowCapture]::GetWindowRect($handle, [ref]$afterKeyboardRect)
        Write-Output ('GUI_GEOMETRY_AFTER_KEYBOARD=' + ($afterKeyboardRect.Right - $afterKeyboardRect.Left) + 'x' + ($afterKeyboardRect.Bottom - $afterKeyboardRect.Top))
        $channelField = Find-NativeControl '*' ([System.Windows.Automation.ControlType]::Edit)
        if (-not $channelField) { throw 'Missing accessible channel input.' }
        $fieldValue = $channelField.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern)
        $fieldValue.SetValue('not-a-youtube-channel')
        Start-Sleep -Milliseconds 150
        Invoke-NativeButton 'Load channel*' -UseRealMouse:$PhysicalPrimaryActions
        $errorVisible = $false
        for ($attempt=0; $attempt -lt 18; $attempt++) {
            $errorControls = $automationRoot.FindAll([System.Windows.Automation.TreeScope]::Descendants, [System.Windows.Automation.Condition]::TrueCondition)
            $errorNames = @($errorControls | ForEach-Object { $_.Current.Name })
            if ($errorNames -contains 'Channel error') { $errorVisible = $true; break }
            Start-Sleep -Milliseconds 150
        }
        if (-not $errorVisible) {
            throw 'GUI interaction: invalid channel URL did not produce an inline error.'
        }
        Write-Output 'GUI_INTERACTION_CHANNEL_ERROR_PASS'
        $afterRect = New-Object WindowCapture+Rect
        if (-not [WindowCapture]::GetWindowRect($handle, [ref]$afterRect)) {
            throw 'Could not read window bounds after GUI interaction.'
        }
        $afterWidth = $afterRect.Right - $afterRect.Left
        $afterHeight = $afterRect.Bottom - $afterRect.Top
        Write-Output ('GUI_INTERACTION_WINDOW_GEOMETRY=' + $width + 'x' + $height + ' -> ' + $afterWidth + 'x' + $afterHeight)
        if ($afterWidth -ne $width -or $afterHeight -ne $height) {
            throw 'GUI interaction unexpectedly changed the native window size.'
        }
        if ($ExerciseLicensedDownload) {
            $licensedTitle = 'Big Buck Bunny - animation short film'
            $licensedCheckbox = Find-NativeControl $licensedTitle ([System.Windows.Automation.ControlType]::CheckBox)
            if (-not $licensedCheckbox) { throw 'Missing licensed test video.' }
            $licensedCheckbox.GetCurrentPattern([System.Windows.Automation.TogglePattern]::Pattern).Toggle()
            Start-Sleep -Milliseconds 200
            $licensedSelected = Get-Content -LiteralPath $sessionFile -Raw | ConvertFrom-Json
            if (@($licensedSelected.videos | Where-Object { $_.selected }).Count -ne 1 -or -not $licensedSelected.videos[2].selected) {
                throw 'Licensed test must select only Big Buck Bunny.'
            }
            Invoke-NativeButton 'Continue*'
            Invoke-NativeButton '360p'
            Invoke-NativeButton 'Start downloads*'
            Write-Output 'GUI_LICENSED_DOWNLOAD_STARTED'
            $finalJob = $null
            for ($attempt=0; $attempt -lt 100; $attempt++) {
                Start-Sleep -Seconds 2
                $currentSession = Get-Content -LiteralPath $sessionFile -Raw | ConvertFrom-Json
                $finalJob = @($currentSession.jobs | Where-Object { $_.video.id -eq 'aqz-KE-bpKQ' -and $_.mode -eq 'video' -and $_.quality -eq '360' }) | Select-Object -Last 1
                if ($finalJob -and $finalJob.state -in @('completed','failed','cancelled')) { break }
            }
            if (-not $finalJob) { throw 'Licensed video job was not queued by the GUI.' }
            Write-Output ('GUI_LICENSED_DOWNLOAD_JOB=' + $finalJob.state + ' ' + $finalJob.detail)
            if ($finalJob.state -ne 'completed') {
                throw ('Licensed video download did not complete: ' + $finalJob.state + ' ' + $finalJob.detail)
            }
            $outputs = @(Get-ChildItem -LiteralPath $functionalOutput -Recurse -File | Where-Object {
                $_.Name -like '*aqz-KE-bpKQ*' -and $_.Extension -in @('.mp4','.mkv','.webm') -and $_.Length -gt 0
            })
            if ($outputs.Count -lt 1) { throw 'Job completed without a verifiable video file.' }
            Write-Output ('GUI_LICENSED_DOWNLOAD_PASS file=' + $outputs[0].FullName + ' bytes=' + $outputs[0].Length)
        }
    }
    if ($ExerciseRealChannelLoad) {
        Add-Type -AssemblyName UIAutomationClient
        Add-Type -AssemblyName UIAutomationTypes
        $channelRoot = [System.Windows.Automation.AutomationElement]::FromHandle($handle)
        $nodes = $channelRoot.FindAll([System.Windows.Automation.TreeScope]::Descendants, [System.Windows.Automation.Condition]::TrueCondition)
        $channelEdit = @($nodes | Where-Object { $_.Current.ControlType.ProgrammaticName -eq 'ControlType.Edit' }) | Select-Object -First 1
        $load = @($nodes | Where-Object { $_.Current.ControlType.ProgrammaticName -eq 'ControlType.Button' -and $_.Current.Name -like 'Load channel*' }) | Select-Object -First 1
        if (-not $channelEdit -or -not $load) { throw 'Missing channel input or Load button for live GUI channel check.' }
        $channelEdit.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern).SetValue($RealChannelUrl)
        Start-Sleep -Milliseconds 250
        $load.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()
        Write-Output ('GUI_REAL_CHANNEL_LOAD_STARTED=' + $RealChannelUrl)
        $sessionFile = Join-Path $previewRoot 'YouTube Downloader Desktop\session.json'
        $loaded = $null
        for ($attempt = 0; $attempt -lt 120; $attempt++) {
            Start-Sleep -Seconds 1
            try {
                $candidate = Get-Content -LiteralPath $sessionFile -Raw | ConvertFrom-Json
                if (@($candidate.videos).Count -ge 5 -and $candidate.channel_name -notin @('', 'Example Creator')) {
                    $loaded = $candidate
                    break
                }
            } catch { }
        }
        if (-not $loaded) {
            $current = $channelRoot.FindAll([System.Windows.Automation.TreeScope]::Descendants, [System.Windows.Automation.Condition]::TrueCondition)
            $labels = @($current | ForEach-Object { $_.Current.Name } | Where-Object { $_ -like '*error*' -or $_ -like '*channel*' -or $_ -like '*yt-dlp*' })
            throw ('Real GUI channel did not load five videos in 120 seconds; UI status: ' + ($labels -join ' | '))
        }
        Write-Output ('GUI_REAL_CHANNEL_LOADED_PASS count=' + @($loaded.videos).Count + ' channel=' + $loaded.channel_name)
        # Session persistence precedes UI model publication in the UI event
        # callback. Wait for the accessibility tree to catch up instead of
        # racing the event loop when a full 200-video channel is loaded.
        $rows = @()
        for ($attempt = 0; $attempt -lt 60; $attempt++) {
            $current = $channelRoot.FindAll([System.Windows.Automation.TreeScope]::Descendants, [System.Windows.Automation.Condition]::TrueCondition)
            $rows = @($current | Where-Object { $_.Current.ControlType.ProgrammaticName -eq 'ControlType.CheckBox' } | Select-Object -First 5)
            if ($rows.Count -ge 5) { break }
            Start-Sleep -Milliseconds 250
        }
        if ($rows.Count -lt 5) { throw ('Loaded channel but fewer than five accessible video checkboxes after model wait: ' + $rows.Count) }
        Write-Output 'GUI_REAL_CHANNEL_ACCESSIBLE_ROWS_PASS'
        foreach ($row in $rows) {
            $toggle = $row.GetCurrentPattern([System.Windows.Automation.TogglePattern]::Pattern)
            if ($toggle.Current.ToggleState -ne [System.Windows.Automation.ToggleState]::On) { $toggle.Toggle() }
            Start-Sleep -Milliseconds 120
        }
        $selected = Get-Content -LiteralPath $sessionFile -Raw | ConvertFrom-Json
        if (@($selected.videos | Where-Object { $_.selected }).Count -ne 5) {
            throw 'GUI failed to persist exactly five selected real channel videos.'
        }
        Write-Output 'GUI_REAL_CHANNEL_SELECT_FIVE_PASS'
        if ($ExerciseRealChannelPagination) {
            # Exercise the Slint ScrollView through actual Win32 wheel events,
            # not UIAutomation's synthetic InvokePattern. Keep the user's live
            # LOCALAPPDATA untouched; this entire app instance is isolated.
            if (@($selected.videos).Count -ne 200) {
                throw 'Pagination fixture requires a channel with at least 200 initial public videos.'
            }
            $wheelPointX = [int]($rect.Left + [Math]::Min(560, $width / 2))
            $wheelPointY = [int]($rect.Top + [Math]::Min(515, $height - 235))
            [void][WindowCapture]::SetForegroundWindow($handle)
            Start-Sleep -Milliseconds 250
            [uint32]$foregroundId = 0
            [void][WindowCapture]::GetWindowThreadProcessId([WindowCapture]::GetForegroundWindow(), [ref]$foregroundId)
            if ($foregroundId -ne $process.Id) { throw 'Scroll paging test app did not own foreground. Refusing physical scroll.' }
            [void][WindowCapture]::SetCursorPos($wheelPointX, $wheelPointY)
            Write-Output ('GUI_REAL_CHANNEL_WHEEL_BEGIN=' + $wheelPointX + ',' + $wheelPointY)
            $wheelDown = [BitConverter]::ToUInt32([BitConverter]::GetBytes([int]-120), 0)
            for ($step = 0; $step -lt 230; $step++) {
                [WindowCapture]::mouse_event(0x0800, 0, 0, $wheelDown, [UIntPtr]::Zero)
                Start-Sleep -Milliseconds 24
            }
            $expanded = $null
            for ($attempt = 0; $attempt -lt 120; $attempt++) {
                Start-Sleep -Seconds 1
                try {
                    $candidate = Get-Content -LiteralPath $sessionFile -Raw | ConvertFrom-Json
                    if (@($candidate.videos).Count -gt 200) { $expanded = $candidate; break }
                } catch { }
            }
            if (-not $expanded) { throw 'Scrolling to the end of 200 videos did not request the next channel page.' }
            if (@($expanded.videos | Where-Object { $_.selected }).Count -ne 5) {
                throw 'Scroll pagination lost the five existing video selections.'
            }
            if (@($expanded.videos).Count -gt 1000) { throw 'Channel pagination exceeded 1000 row cap.' }
            Write-Output ('GUI_REAL_CHANNEL_SCROLL_PAGINATION_PASS total=' + @($expanded.videos).Count + ' selected=5')
        }
        $current = $channelRoot.FindAll([System.Windows.Automation.TreeScope]::Descendants, [System.Windows.Automation.Condition]::TrueCondition)
        $continue = @($current | Where-Object { $_.Current.ControlType.ProgrammaticName -eq 'ControlType.Button' -and $_.Current.Name -like 'Continue*' }) | Select-Object -First 1
        if (-not $continue) { throw 'Missing Continue after selecting five real channel videos.' }
        $continue.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()
        $visible = $false
        for ($attempt=0; $attempt -lt 15; $attempt++) {
            Start-Sleep -Milliseconds 200
            $current = $channelRoot.FindAll([System.Windows.Automation.TreeScope]::Descendants, [System.Windows.Automation.Condition]::TrueCondition)
            $names = @($current | ForEach-Object { $_.Current.Name })
            if ($names -contains 'Output preferences' -or $names -contains 'Download settings') { $visible = $true; break }
        }
        if (-not $visible) { throw 'Continue did not navigate to Download Options after real channel load.' }
        Write-Output 'GUI_REAL_CHANNEL_OPTIONS_NAV_PASS'
        Write-Output 'GUI_REAL_CHANNEL_NO_DOWNLOAD_STARTED=true'
    }
    if ($VerifyRestoredSession) {
        Add-Type -AssemblyName UIAutomationClient
        Add-Type -AssemblyName UIAutomationTypes
        $restored = Get-Content -LiteralPath (Join-Path $previewRoot 'YouTube Downloader Desktop\session.json') -Raw | ConvertFrom-Json
        $restorationRoot = [System.Windows.Automation.AutomationElement]::FromHandle($handle)
        if ($restored.theme -notin @('red', 'mono')) { throw 'Restored session contains an invalid theme.' }
        $elements = $restorationRoot.FindAll([System.Windows.Automation.TreeScope]::Descendants, [System.Windows.Automation.Condition]::TrueCondition)
        $verifiedRows = 0
        foreach ($savedRow in @($restored.videos)) {
            $matching = @($elements | Where-Object {
                $_.Current.ControlType.ProgrammaticName -eq 'ControlType.CheckBox' -and
                $_.Current.Name -eq $savedRow.video.title
            })
            if ($matching.Count -eq 0) { continue } # Slint may virtualize off-screen video rows.
            if ($matching.Count -ne 1) { throw ('Restored GUI has duplicate checkboxes for ' + $savedRow.video.title) }
            $pattern = $matching[0].GetCurrentPattern([System.Windows.Automation.TogglePattern]::Pattern)
            $checked = $pattern.Current.ToggleState -eq [System.Windows.Automation.ToggleState]::On
            if ($checked -ne [bool]$savedRow.selected) {
                throw ('GUI restored selection mismatch for ' + $savedRow.video.title)
            }
            $verifiedRows++
        }
        if ($verifiedRows -lt 1) { throw 'No saved selections were exposed for GUI restoration verification.' }
        Write-Output ('GUI_RESTORED_THEME_PERSISTED_PASS=' + $restored.theme)
        Write-Output ('GUI_RESTORED_SELECTION_VISIBLE_PASS=' + $verifiedRows + '/' + @($restored.videos).Count)
    }
    if ($ExerciseWindowToggle) {
        Add-Type -AssemblyName UIAutomationClient
        Add-Type -AssemblyName UIAutomationTypes
        Add-Type -AssemblyName System.Windows.Forms
        $windowRoot = [System.Windows.Automation.AutomationElement]::FromHandle($handle)
        function Invoke-WindowModeButton([string]$label) {
            $nodes = $windowRoot.FindAll([System.Windows.Automation.TreeScope]::Descendants, [System.Windows.Automation.Condition]::TrueCondition)
            $button = @($nodes | Where-Object { $_.Current.ControlType.ProgrammaticName -eq 'ControlType.Button' -and $_.Current.Name -eq $label }) | Select-Object -First 1
            if (-not $button) { throw ('Missing window-mode control: ' + $label) }
            $button.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()
            Start-Sleep -Milliseconds 350
        }
        $beforeToggle = New-Object WindowCapture+Rect
        if (-not [WindowCapture]::GetWindowRect($handle, [ref]$beforeToggle)) { throw 'Cannot measure before fullscreen toggle.' }
        Invoke-WindowModeButton 'Fullscreen'
        $duringToggle = New-Object WindowCapture+Rect
        if (-not [WindowCapture]::GetWindowRect($handle, [ref]$duringToggle)) { throw 'Cannot measure fullscreen window.' }
        $screen = [Windows.Forms.Screen]::FromHandle($handle)
        $captionFlag = [WindowCapture]::GetWindowLong($handle, -16) -band 0x00C00000
        if ($captionFlag -ne 0 -or
            ($duringToggle.Right - $duringToggle.Left) -lt ($screen.Bounds.Width - 5) -or
            ($duringToggle.Bottom - $duringToggle.Top) -lt ($screen.Bounds.Height - 5)) {
            throw 'Fullscreen button failed to enter actual monitor-filling borderless mode.'
        }
        Write-Output 'GUI_FULLSCREEN_BUTTON_PASS'
        Invoke-WindowModeButton 'Exit full'
        $afterToggle = New-Object WindowCapture+Rect
        if (-not [WindowCapture]::GetWindowRect($handle, [ref]$afterToggle)) { throw 'Cannot measure restored window.' }
        if ([Math]::Abs(($afterToggle.Right - $afterToggle.Left) - ($beforeToggle.Right - $beforeToggle.Left)) -gt 2 -or
            [Math]::Abs(($afterToggle.Bottom - $afterToggle.Top) - ($beforeToggle.Bottom - $beforeToggle.Top)) -gt 2) {
            throw 'Exit full failed to restore the previous normal window size.'
        }
        Write-Output 'GUI_FULLSCREEN_RESTORE_SIZE_PASS'
    }
    if ($FocusPreview) {
        if ($Preview -ne 'library' -or $EmptyState) { throw 'FocusPreview requires a populated library.' }
        Add-Type -AssemblyName UIAutomationClient
        Add-Type -AssemblyName UIAutomationTypes
        $focusRoot = [System.Windows.Automation.AutomationElement]::FromHandle($handle)
        $focusRows = $focusRoot.FindAll([System.Windows.Automation.TreeScope]::Descendants, [System.Windows.Automation.Condition]::TrueCondition)
        $focusRow = @($focusRows | Where-Object { $_.Current.ControlType.ProgrammaticName -eq 'ControlType.CheckBox' -and $_.Current.Name -like 'Rick Astley*' }) | Select-Object -First 1
        if (-not $focusRow) { throw 'Accessible first video not found.' }
        $focusRow.SetFocus()
        Start-Sleep -Milliseconds 350
        Write-Output ('GUI_FOCUS_PREVIEW=' + [System.Windows.Automation.AutomationElement]::FocusedElement.Current.Name)
    }
    if ($ExerciseTitlePopup) {
        Add-Type -AssemblyName UIAutomationClient
        Add-Type -AssemblyName UIAutomationTypes
        $popupRoot = [System.Windows.Automation.AutomationElement]::FromHandle($handle)
        $buttonSet = $popupRoot.FindAll([System.Windows.Automation.TreeScope]::Descendants, [System.Windows.Automation.Condition]::TrueCondition)
        $titleButton = @($buttonSet | Where-Object { $_.Current.ControlType.ProgrammaticName -eq 'ControlType.Button' -and $_.Current.Name -eq 'Full title' }) | Select-Object -First 1
        if (-not $titleButton) { throw 'Long-title disclosure button missing from the accessibility tree.' }
        $titleButton.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()
        Start-Sleep -Milliseconds 350
        $popupFound = $false
        for ($attempt=0; $attempt -lt 15; $attempt++) {
            $current = $popupRoot.FindAll([System.Windows.Automation.TreeScope]::Descendants, [System.Windows.Automation.Condition]::TrueCondition)
            $names = @($current | ForEach-Object { $_.Current.Name })
            if ($names -contains 'Hide title') { $popupFound = $true; break }
            Start-Sleep -Milliseconds 150
        }
        if (-not $popupFound) { throw 'Full-title disclosure did not expand its row.' }
        Write-Output 'GUI_INLINE_TITLE_DISCLOSURE_PASS'
        if (-not $CaptureExpandedTitle) {
            $hide = @($current | Where-Object { $_.Current.Name -eq 'Hide title' -and $_.Current.ControlType.ProgrammaticName -eq 'ControlType.Button' }) | Select-Object -First 1
            if (-not $hide) { throw 'Expanded full title cannot be collapsed.' }
            $hide.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()
            Start-Sleep -Milliseconds 280
            $afterHide = $popupRoot.FindAll([System.Windows.Automation.TreeScope]::Descendants, [System.Windows.Automation.Condition]::TrueCondition)
            if (@($afterHide | ForEach-Object { $_.Current.Name }) -notcontains 'Full title') {
                throw 'Full title did not return to the collapsed state.'
            }
            Write-Output 'GUI_INLINE_TITLE_COLLAPSE_PASS'
        }
    }
    if ($ExerciseSelectionPopup) {
        # This switch also supports older callers of the setup review check.
        Add-Type -AssemblyName UIAutomationClient
        Add-Type -AssemblyName UIAutomationTypes
        $selectionRoot = [System.Windows.Automation.AutomationElement]::FromHandle($handle)
        $visible = $false
        for ($attempt = 0; $attempt -lt 18; $attempt++) {
            $items = $selectionRoot.FindAll([System.Windows.Automation.TreeScope]::Descendants, [System.Windows.Automation.Condition]::TrueCondition)
            $names = @($items | ForEach-Object { $_.Current.Name })
            $quality = @($items | Where-Object { $_.Current.Name -eq '720p' -and $_.Current.ControlType.ProgrammaticName -eq 'ControlType.Button' }) | Select-Object -First 1
            $start = @($items | Where-Object { $_.Current.Name -like 'Start downloads*' -and $_.Current.ControlType.ProgrammaticName -eq 'ControlType.Button' }) | Select-Object -First 1
            if ($names -contains 'Your selection' -and
                $names -contains 'Rick Astley - Never Gonna Give You Up' -and
                $quality -and $start -and $quality.Current.BoundingRectangle.Height -ge 30 -and
                $quality.Current.BoundingRectangle.Bottom -lt $start.Current.BoundingRectangle.Top) {
                $visible = $true
                break
            }
            Start-Sleep -Milliseconds 120
        }
        if (-not $visible) { throw 'The selected title and quality control are not visible together above Start downloads.' }
        Write-Output 'GUI_SELECTION_REVIEW_PASS'
    }
    if ($CaptureHistoryConfirmation) {
        Add-Type -AssemblyName UIAutomationClient
        Add-Type -AssemblyName UIAutomationTypes
        $historyRoot = [System.Windows.Automation.AutomationElement]::FromHandle($handle)
        $all = $historyRoot.FindAll([System.Windows.Automation.TreeScope]::Descendants, [System.Windows.Automation.Condition]::TrueCondition)
        $clearButton = @($all | Where-Object { $_.Current.Name -eq 'Clear history' -and $_.Current.ControlType.ProgrammaticName -eq 'ControlType.Button' }) | Select-Object -First 1
        if (-not $clearButton) { throw 'Missing Clear history for visual confirmation test.' }
        $clearButton.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()
        Start-Sleep -Milliseconds 350
        $controls = $historyRoot.FindAll([System.Windows.Automation.TreeScope]::Descendants, [System.Windows.Automation.Condition]::TrueCondition)
        $names = @($controls | ForEach-Object { $_.Current.Name })
        if ($names -notcontains 'Clear finished download history?' -or $names -notcontains 'Clear records' -or $names -notcontains 'Cancel') {
            throw 'Clear history confirmation controls are not accessible.'
        }
        Write-Output 'GUI_CLEAR_HISTORY_CONFIRMATION_VISIBLE_PASS'
    }
    if ($ExerciseQueueHistory) {
        Add-Type -AssemblyName UIAutomationClient
        Add-Type -AssemblyName UIAutomationTypes
        $queueRoot = [System.Windows.Automation.AutomationElement]::FromHandle($handle)
        $all = $queueRoot.FindAll([System.Windows.Automation.TreeScope]::Descendants, [System.Windows.Automation.Condition]::TrueCondition)
        $cancelAll = @($all | Where-Object { $_.Current.Name -eq 'Cancel active & queued' -and $_.Current.ControlType.ProgrammaticName -eq 'ControlType.Button' }) | Select-Object -First 1
        $retryAll = @($all | Where-Object { $_.Current.Name -eq 'Retry failed' -and $_.Current.ControlType.ProgrammaticName -eq 'ControlType.Button' }) | Select-Object -First 1
        Write-Output ('GUI_QUEUE_UIA_ACTION_STATES cancelPresent=' + [bool]$cancelAll + ' cancelEnabled=' + $(if($cancelAll){$cancelAll.Current.IsEnabled}else{'missing'}) +
            ' retryPresent=' + [bool]$retryAll + ' retryEnabled=' + $(if($retryAll){$retryAll.Current.IsEnabled}else{'missing'}))
        if (-not $cancelAll -or -not $retryAll -or -not $retryAll.Current.IsEnabled) {
            throw 'Idle queue action state is incorrect: Cancel/Retry buttons missing or Retry unavailable.'
        }
        if ($cancelAll.Current.IsEnabled) {
            Write-Output 'GUI_QUEUE_DISABLED_ACCESSIBILITY_FAIL=Cancel active & queued reports UIA enabled on idle queue'
        } else {
            Write-Output 'GUI_QUEUE_IDLE_ACTION_STATE_PASS'
        }
        $openFolder = @($all | Where-Object { $_.Current.Name -eq 'Open folder' -and $_.Current.ControlType.ProgrammaticName -eq 'ControlType.Button' }) | Select-Object -First 1
        if (-not $openFolder) { throw 'Missing completed-job Open folder button.' }
        $openFolder.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()
        $errorVisible = $false
        for ($attempt = 0; $attempt -lt 20; $attempt++) {
            Start-Sleep -Milliseconds 120
            $nodes = $queueRoot.FindAll([System.Windows.Automation.TreeScope]::Descendants, [System.Windows.Automation.Condition]::TrueCondition)
            if (@($nodes | ForEach-Object { $_.Current.Name }) -contains 'Action needed: The download folder is no longer available.') {
                $errorVisible = $true; break
            }
        }
        if (-not $errorVisible) { throw 'Missing folder error was not displayed in Downloads.' }
        Write-Output 'GUI_QUEUE_INLINE_ERROR_PASS'
        $nodes = $queueRoot.FindAll([System.Windows.Automation.TreeScope]::Descendants, [System.Windows.Automation.Condition]::TrueCondition)
        $clear = @($nodes | Where-Object { $_.Current.Name -eq 'Clear history' -and $_.Current.ControlType.ProgrammaticName -eq 'ControlType.Button' }) | Select-Object -First 1
        if (-not $clear) { throw 'Missing Clear history button.' }
        $clear.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()
        $sessionFile = Join-Path $previewRoot 'YouTube Downloader Desktop\session.json'
        $saved = Get-Content -LiteralPath $sessionFile -Raw | ConvertFrom-Json
        if (@($saved.jobs).Count -ne 3) { throw 'Clear history deleted jobs without confirmation.' }
        Start-Sleep -Milliseconds 250
        $nodes = $queueRoot.FindAll([System.Windows.Automation.TreeScope]::Descendants, [System.Windows.Automation.Condition]::TrueCondition)
        $cancel = @($nodes | Where-Object { $_.Current.Name -eq 'Cancel' -and $_.Current.ControlType.ProgrammaticName -eq 'ControlType.Button' }) | Select-Object -First 1
        if (-not $cancel) { throw 'Clear history confirmation lacks Cancel.' }
        $cancel.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()
        Start-Sleep -Milliseconds 250
        $saved = Get-Content -LiteralPath $sessionFile -Raw | ConvertFrom-Json
        if (@($saved.jobs).Count -ne 3) { throw 'Cancel unexpectedly removed history.' }
        Write-Output 'GUI_QUEUE_CLEAR_CANCEL_PRESERVES_DATA_PASS'
        $nodes = $queueRoot.FindAll([System.Windows.Automation.TreeScope]::Descendants, [System.Windows.Automation.Condition]::TrueCondition)
        $clear = @($nodes | Where-Object { $_.Current.Name -eq 'Clear history' -and $_.Current.ControlType.ProgrammaticName -eq 'ControlType.Button' }) | Select-Object -First 1
        $clear.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()
        Start-Sleep -Milliseconds 250
        $nodes = $queueRoot.FindAll([System.Windows.Automation.TreeScope]::Descendants, [System.Windows.Automation.Condition]::TrueCondition)
        $confirm = @($nodes | Where-Object { $_.Current.Name -eq 'Clear records' -and $_.Current.ControlType.ProgrammaticName -eq 'ControlType.Button' }) | Select-Object -First 1
        if (-not $confirm) { throw 'Clear history confirmation lacks Clear records.' }
        $confirm.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern).Invoke()
        Start-Sleep -Milliseconds 350
        $saved = Get-Content -LiteralPath $sessionFile -Raw | ConvertFrom-Json
        if (@($saved.jobs).Count -ne 0) { throw 'Confirmed Clear history did not persist an empty queue.' }
        $nodes = $queueRoot.FindAll([System.Windows.Automation.TreeScope]::Descendants, [System.Windows.Automation.Condition]::TrueCondition)
        $names = @($nodes | ForEach-Object { $_.Current.Name })
        if ($names -notcontains 'Your download queue is empty' -and $names -notcontains 'No downloads yet') {
            throw 'Clear history did not show the empty queue.'
        }
        Write-Output 'GUI_QUEUE_CLEAR_HISTORY_PASS'
    }
    if ($VerifyEmptyQueue) {
        Add-Type -AssemblyName UIAutomationClient
        Add-Type -AssemblyName UIAutomationTypes
        $queueRoot = [System.Windows.Automation.AutomationElement]::FromHandle($handle)
        $nodes = $queueRoot.FindAll([System.Windows.Automation.TreeScope]::Descendants, [System.Windows.Automation.Condition]::TrueCondition)
        $names = @($nodes | ForEach-Object { $_.Current.Name })
        if ($names -notcontains 'Your download queue is empty' -and $names -notcontains 'No downloads yet') {
            throw 'Empty Downloads view lacks the empty-state message.'
        }
        foreach ($unexpected in @('Retry failed', 'Cancel active & queued', 'Clear history', 'All files', 'In progress')) {
            if ($names -contains $unexpected) { throw ('Empty Downloads view exposes an irrelevant control: ' + $unexpected) }
        }
        Write-Output 'GUI_EMPTY_QUEUE_REDUCED_CONTROLS_PASS'
    }
    if ($ProbeAccessibility) {
        Add-Type -AssemblyName UIAutomationClient
        Add-Type -AssemblyName UIAutomationTypes
        $automationRoot = [System.Windows.Automation.AutomationElement]::FromHandle($handle)
        $accessible = $automationRoot.FindAll(
            [System.Windows.Automation.TreeScope]::Descendants,
            [System.Windows.Automation.Condition]::TrueCondition
        )
        $named = @()
        foreach ($element in $accessible) {
            try {
                $name = $element.Current.Name
                if (-not [string]::IsNullOrWhiteSpace($name)) {
                    $named += ('{0}:{1}' -f $element.Current.ControlType.ProgrammaticName, $name)
                    if ($name -eq 'Mono' -or $name -like 'Continue*' -or $name -like 'Rick Astley*') {
                        $patterns = @($element.GetSupportedPatterns() | ForEach-Object { $_.ProgrammaticName })
                        Write-Output ('GUI_ACCESSIBILITY_PATTERNS=' + $name + ':' + ($patterns -join ','))
                        Write-Output ('GUI_ACCESSIBILITY_FOCUSABLE=' + $name + ':' + $element.Current.IsKeyboardFocusable)
                    }
                }
            } catch { }
        }
        Write-Output ('GUI_ACCESSIBILITY_NODES=' + $accessible.Count)
        Write-Output ('GUI_ACCESSIBILITY_NAMED=' + ($named -join ' | '))
    }
    if ($WindowMode -eq 'maximized' -and -not [WindowCapture]::IsZoomed($handle)) {
        throw 'Maximize requested, but the native window is not maximized.'
    }
    if ($WindowMode -eq 'fullscreen') {
        Add-Type -AssemblyName System.Windows.Forms
        $screen = [Windows.Forms.Screen]::FromHandle($handle)
        $captionFlag = [WindowCapture]::GetWindowLong($handle, -16) -band 0x00C00000
        if ($captionFlag -ne 0) { throw 'Fullscreen window still has a title bar.' }
        if ($width -lt $screen.Bounds.Width - 5 -or $height -lt $screen.Bounds.Height - 5) {
            throw ('Fullscreen did not cover monitor: '+$width+'x'+$height)
        }
    }
    $live = Get-Process -Id $process.Id
    Write-Output ('GUI_SMOKE_PASS window=' + $width + 'x' + $height + ' workingSetMiB=' + [Math]::Round($live.WorkingSet64/1MB, 1))
    $suffix = if ($ReuseSession) { "restored-$Preview-$WindowMode" } else { "$Theme-$Preview-$WindowMode" + $(if($EmptyState){'-empty'}else{''}) + $(if($ErrorPreview -ne 'none'){'-error'}else{''}) + $(if($ExerciseControls){'-interaction'}else{''}) + $(if($LongTitles){'-long'}else{''}) + $(if($FocusPreview){'-focus'}else{''}) + $(if($ExerciseTitlePopup){'-title'}else{''}) + $(if($ExerciseSelectionPopup){'-review'}else{''}) + $(if($ExerciseQueueHistory){'-history'}else{''}) }
    New-Item -ItemType Directory -Path $previewRoot -Force | Out-Null
    $captureId = [guid]::NewGuid().ToString('N').Substring(0, 8)
    $image = Join-Path $previewRoot ("window-smoke-{0}-{1}.png" -f $suffix, $captureId)
    $bitmap = New-Object System.Drawing.Bitmap($width, $height)
    try {
        $graphics = [Drawing.Graphics]::FromImage($bitmap)
        try {
            $hdc = $graphics.GetHdc()
            try {
                if (-not [WindowCapture]::PrintWindow($handle, $hdc, 2)) {
                    throw 'Windows refused off-screen rendering for this window.'
                }
            } finally { $graphics.ReleaseHdc($hdc) }
        } finally { $graphics.Dispose() }
        $pixel = $bitmap.GetPixel([int]($width / 2), [int]($height / 2))
        if ($pixel.R -lt 4 -and $pixel.G -lt 4 -and $pixel.B -lt 4) {
            # GPU-backed popups can sometimes return a black frame through
            # PW_RENDERFULLCONTENT. Retry the standard Win32 capture mode.
            $retryGraphics = [Drawing.Graphics]::FromImage($bitmap)
            try {
                $retryHdc = $retryGraphics.GetHdc()
                try { [void][WindowCapture]::PrintWindow($handle, $retryHdc, 0) }
                finally { $retryGraphics.ReleaseHdc($retryHdc) }
            } finally { $retryGraphics.Dispose() }
            $pixel = $bitmap.GetPixel([int]($width / 2), [int]($height / 2))
        }
        if ($pixel.R -lt 4 -and $pixel.G -lt 4 -and $pixel.B -lt 4) {
            # Only capture screen pixels when our test application actually
            # owns the foreground window (including its native popup).
            $fg = [WindowCapture]::GetForegroundWindow()
            [uint32]$fgPid = 0
            if ($fg -ne [IntPtr]::Zero) {
                [void][WindowCapture]::GetWindowThreadProcessId($fg, [ref]$fgPid)
            }
            Write-Output ('GUI_OFFSCREEN_BLACK_FOREGROUND_PID=' + $fgPid + ' app_pid=' + $process.Id)
            if ($fgPid -eq $process.Id -and -not [WindowCapture]::IsIconic($handle)) {
                $screenBounds = New-Object WindowCapture+Rect
                if ([WindowCapture]::GetWindowRect($handle, [ref]$screenBounds)) {
                    $copyGraphics = [Drawing.Graphics]::FromImage($bitmap)
                    try {
                        $copyGraphics.CopyFromScreen($screenBounds.Left, $screenBounds.Top, 0, 0, (New-Object Drawing.Size($width, $height)))
                    } finally { $copyGraphics.Dispose() }
                    $pixel = $bitmap.GetPixel([int]($width / 2), [int]($height / 2))
                    Write-Output 'GUI_SCREENSHOT_FOREGROUND_CAPTURE_USED'
                }
            }
        }
        if ($pixel.R -lt 4 -and $pixel.G -lt 4 -and $pixel.B -lt 4) {
            throw 'Native screenshot is entirely black in the viewport; capture is invalid.'
        }
        $latestAtCapture = Get-NewestBetaSourceTimestamp
        if ($latestAtCapture -gt $buildTimestamp) {
            Write-Output 'GUI_CAPTURE_STALE_SOURCE_CHANGED=true'
            if (-not $AllowStaleExecutable) { throw 'Source changed after the executable was built; screenshot cannot be treated as current beta evidence.' }
        }
        if ((Get-FileHash -LiteralPath $Executable -Algorithm SHA256).Hash -ne $buildHash) {
            throw 'Executable changed during GUI smoke run; screenshot provenance is invalid.'
        }
        $bitmap.Save($image, [Drawing.Imaging.ImageFormat]::Png)
        Write-Output ('GUI_SCREENSHOT=' + $image)
    } catch {
        throw ('GUI_SCREENSHOT_FAILED: ' + $_.Exception.Message)
    } finally { $bitmap.Dispose() }
} finally {
    $existing = Get-Process -Id $process.Id -ErrorAction SilentlyContinue
    if ($existing) { Stop-Process -Id $process.Id -ErrorAction Stop }
}

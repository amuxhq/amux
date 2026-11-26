#Requires -Version 5.1
# Installs amux:  irm https://amux.sh/install | iex
#
# The stable channel's manifest is the one document that says which build
# each machine should run. This script reads it, takes this machine's
# entry, downloads the binary the entry names, and installs it only if its
# size and sha256 are the entry's. A machine installs whatever the channel
# names, whatever share of running machines a rollout has reached: the
# rollout protects machines that already run something.
#
# Beside amux.exe goes amux-launch.exe, the windowless launcher Windows'
# login item runs, from the manifest's entry beside the target's
# ("<target>/amux-launch"), checked the same way. A manifest that names no
# launcher of the same release predates it, and amux alone is installed.
#
# The manifest is signed, and an installed amux checks that signature on
# every later update with the key it was built with. This script does not:
# it and the manifest come from the same place, so a check here would
# trust the server it was checking. What it does establish is that each
# download is the file the release names.
#
#   AMUX_RELEASES_URL     where the manifests are (default https://amux.sh/releases)
#   AMUX_NO_MODIFY_PATH   set to leave the user PATH alone

$ErrorActionPreference = "Stop"
# Windows PowerShell draws a progress bar for every chunk a download
# reads, which makes it many times slower.
$ProgressPreference = "SilentlyContinue"
# Windows PowerShell 5.1 may offer only TLS 1.0 and 1.1, which the release
# hosts refuse.
if ($PSVersionTable.PSVersion.Major -lt 6) {
    [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
}

$ReleasesUrl = if ($env:AMUX_RELEASES_URL) { $env:AMUX_RELEASES_URL } else { "https://amux.sh/releases" }
$InstallDir = Join-Path $env:USERPROFILE ".amux\bin"
$BinaryName = "amux.exe"
$LauncherName = "amux-launch.exe"

# The target as the manifest names it: the build's Rust target triple.
# Only x64 ships for Windows; Windows on Arm runs it emulated.
function Get-Target {
    $arch = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture

    switch ($arch) {
        "X64" { return "x86_64-pc-windows-msvc" }
        "Arm64" { return "x86_64-pc-windows-msvc" }
        default { throw "Unsupported architecture: $arch." }
    }
}

# The hex sha256 of a file. Hashed with .NET rather than Get-FileHash: that
# cmdlet is a script function Windows PowerShell cannot find when it is
# started from PowerShell 7, whose module path it inherits.
function Get-Sha256($Path) {
    $stream = [System.IO.File]::OpenRead($Path)
    try {
        $hash = [System.Security.Cryptography.SHA256]::Create().ComputeHash($stream)
    }
    finally {
        $stream.Dispose()
    }
    return -join ($hash | ForEach-Object { $_.ToString("x2") })
}

function Assert-Checksum($Path, $Expected) {
    $actual = Get-Sha256 $Path
    if ($actual -ne $Expected) {
        throw @"
Checksum verification failed for $(Split-Path -Leaf $Path).
  Expected: $Expected
  Actual:   $actual
"@
    }
}

# The download at $Path is the file the manifest entry $Entry names: its
# size, then its sha256.
function Assert-Download($Path, $Entry) {
    $actualSize = (Get-Item $Path).Length
    if ($actualSize -ne [int64]$Entry.size) {
        throw "The download of $(Split-Path -Leaf $Path) is $actualSize bytes; the manifest says $($Entry.size)."
    }
    Assert-Checksum $Path $Entry.sha256
}

# Runs $Action, again for up to about five seconds while it fails: a fresh
# executable is briefly held by the virus scanner reading it.
function Invoke-Patiently([scriptblock]$Action) {
    $pause = 50
    for ($try = 1; ; $try++) {
        try {
            & $Action
            return
        }
        catch {
            if ($try -ge 8) { throw }
            Start-Sleep -Milliseconds $pause
            $pause = [Math]::Min($pause * 2, 1000)
        }
    }
}

# A name beside $Path no file has: $Path.old.<n>.
function Get-FreshAside($Path) {
    for ($n = 1; ; $n++) {
        $aside = "$Path.old.$n"
        if (-not (Test-Path -LiteralPath $aside)) { return $aside }
    }
}

# Puts $Source at $Destination. A running program can't be deleted or
# replaced on Windows, but it can be renamed: one still held is moved to a
# fresh .old.<n> beside it, which amux removes once nothing runs it.
function Install-File($Source, $Destination) {
    if (Test-Path -LiteralPath $Destination) {
        try {
            Remove-Item -LiteralPath $Destination -Force
        }
        catch {
            $aside = Get-FreshAside $Destination
            Invoke-Patiently { Rename-Item -LiteralPath $Destination -NewName (Split-Path -Leaf $aside) }
            Write-Host "$(Split-Path -Leaf $Destination) is in use, so it was moved aside to $(Split-Path -Leaf $aside)."
        }
    }
    Invoke-Patiently { Move-Item -LiteralPath $Source -Destination $Destination }
}

# Deletes the programs an earlier install or update moved aside, leaving
# any still in use for next time.
function Remove-SetAside {
    foreach ($name in $BinaryName, $LauncherName) {
        Get-ChildItem -LiteralPath $InstallDir -Filter "$name.old*" -Force -ErrorAction SilentlyContinue |
            ForEach-Object { Remove-Item -LiteralPath $_.FullName -Force -ErrorAction SilentlyContinue }
    }
}

# The installed amux, when one is running from the install folder: amux
# supervise, its daemon or the launcher.
function Test-Running {
    $programs = @((Join-Path $InstallDir $BinaryName), (Join-Path $InstallDir $LauncherName))
    $running = Get-Process -Name "amux", "amux-launch" -ErrorAction SilentlyContinue |
        Where-Object { $_.Path -and ($programs -contains $_.Path) }
    return [bool]$running
}

# Tells every window the user's environment changed, as the System control
# panel does, so Explorer reads it again and what it starts next has the
# new PATH.
function Send-EnvironmentChange {
    if (-not ("AmuxInstall.Native" -as [type])) {
        Add-Type -Namespace AmuxInstall -Name Native -MemberDefinition @'
[DllImport("user32.dll", CharSet = CharSet.Unicode)]
public static extern IntPtr SendMessageTimeout(IntPtr hWnd, uint msg, UIntPtr wParam, string lParam, uint flags, uint timeout, out UIntPtr result);
'@
    }
    $result = [UIntPtr]::Zero
    # HWND_BROADCAST, WM_SETTINGCHANGE, SMTO_ABORTIFHUNG, five seconds.
    [void][AmuxInstall.Native]::SendMessageTimeout([IntPtr]0xffff, 0x1A, [UIntPtr]::Zero, "Environment", 2, 5000, [ref]$result)
}

# Puts $Dir first on the user's own PATH, the Path value of
# HKCU\Environment, unless it is on it already. The value is read and
# written as it is stored, a REG_EXPAND_SZ whose %VARIABLES% stay
# unexpanded; [Environment]::SetEnvironmentVariable would expand them for
# good. AMUX_TEST_ENVIRONMENT_KEY names a test's key in its place.
function Add-UserPath($Dir) {
    $keyName = "Environment"
    if ($env:AMUX_TEST_ENVIRONMENT_KEY) {
        $keyName = $env:AMUX_TEST_ENVIRONMENT_KEY
        if (-not $keyName.StartsWith("Software\amux-test-")) {
            throw "AMUX_TEST_ENVIRONMENT_KEY must name a key under Software\amux-test-, not $keyName."
        }
    }
    $key = [Microsoft.Win32.Registry]::CurrentUser.CreateSubKey($keyName)
    try {
        $current = [string]$key.GetValue("Path", "", [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
        $wanted = $Dir.TrimEnd("\")
        foreach ($entry in ($current -split ";")) {
            $entry = $entry.Trim().Trim('"')
            if ($entry -and [Environment]::ExpandEnvironmentVariables($entry).TrimEnd("\") -ieq $wanted) {
                return $false
            }
        }
        $value = if ($current) { "$Dir;$current" } else { $Dir }
        $key.SetValue("Path", $value, [Microsoft.Win32.RegistryValueKind]::ExpandString)
    }
    finally {
        $key.Close()
    }
    Send-EnvironmentChange
    return $true
}

function Install-Amux {
    $target = Get-Target
    $arch = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture
    Write-Host "Detected architecture: $arch"
    if ("$arch" -eq "Arm64") {
        Write-Host "Installing the x64 build, which Windows on Arm runs emulated."
    }

    Write-Host "Reading the stable channel..."
    $manifest = Invoke-RestMethod -Uri "$ReleasesUrl/stable.json" -Headers @{ "User-Agent" = "amux-installer" }
    $property = $manifest.targets.PSObject.Properties[$target]
    if (-not $property) {
        throw "The stable channel has no build for $target."
    }
    $entry = $property.Value
    Write-Host "Stable is amux $($entry.version)"

    $tmpDir = Join-Path ([System.IO.Path]::GetTempPath()) "amux-install-$([System.Guid]::NewGuid().ToString('N').Substring(0, 8))"
    New-Item -ItemType Directory -Path $tmpDir -Force | Out-Null

    try {
        $binaryPath = Join-Path $tmpDir $BinaryName

        Write-Host "Downloading $($entry.url)..."
        Invoke-WebRequest -Uri $entry.url -OutFile $binaryPath -UseBasicParsing

        Assert-Download $binaryPath $entry

        # The launcher's entry, beside the target's, when the manifest names
        # one from the same release.
        $launcherProperty = $manifest.targets.PSObject.Properties["$target/amux-launch"]
        $launcherPath = $null
        if ($launcherProperty -and $launcherProperty.Value.version -eq $entry.version) {
            $launcher = $launcherProperty.Value
            $launcherPath = Join-Path $tmpDir $LauncherName
            Write-Host "Downloading $($launcher.url)..."
            Invoke-WebRequest -Uri $launcher.url -OutFile $launcherPath -UseBasicParsing
            Assert-Download $launcherPath $launcher
        }
        else {
            Write-Host "This release ships no login launcher; amux alone is installed."
        }

        Write-Host "Checksums verified."

        New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
        # amux running from here keeps running the old build, and a
        # running program can't be replaced: stop it first.
        $wasRunning = Test-Running
        if ($wasRunning) {
            Write-Host "Stopping the running amux (amux server stop)..."
            & (Join-Path $InstallDir $BinaryName) server stop
        }
        Remove-SetAside

        Write-Host "Installing to $InstallDir\$BinaryName..."
        Install-File $binaryPath (Join-Path $InstallDir $BinaryName)
        if ($launcherPath) {
            Install-File $launcherPath (Join-Path $InstallDir $LauncherName)
        }

        if (-not $env:AMUX_NO_MODIFY_PATH) {
            if (Add-UserPath $InstallDir) {
                Write-Host "Added $InstallDir to your PATH."
            }
            # This session too, so `amux` runs here without a new terminal.
            if (-not (($env:Path -split ";") -contains $InstallDir)) {
                $env:Path = "$InstallDir;$env:Path"
            }
        }

        Write-Host ""
        Write-Host "amux $($entry.version) has been installed successfully!" -ForegroundColor Green
        Write-Host ""
        if ($wasRunning) {
            Write-Host "amux was running, and is stopped now. To start the new build, run:"
            Write-Host "  amux server start"
        }
        else {
            Write-Host "To get started, run:"
            Write-Host "  amux"
        }
    }
    finally {
        Remove-Item -Path $tmpDir -Recurse -Force -ErrorAction SilentlyContinue
    }
}

Install-Amux

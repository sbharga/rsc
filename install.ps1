# Install rsc from GitHub Releases (Windows PowerShell 5.1+).
#
#   powershell -ExecutionPolicy Bypass -c "irm https://raw.githubusercontent.com/sbharga/rsc/master/install.ps1 | iex"
#
# The SHA-256 is always checked; the Sigstore signature is checked too when cosign is on PATH.
#
# Environment:
#   RSC_VERSION      release tag to install (default: newest release, pre-releases included)
#   RSC_INSTALL_DIR  install directory (default: %LOCALAPPDATA%\Programs\rsc)
& {
    $ErrorActionPreference = 'Stop'
    $ProgressPreference = 'SilentlyContinue'
    [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

    $arch = if ($env:PROCESSOR_ARCHITEW6432) { $env:PROCESSOR_ARCHITEW6432 } else { $env:PROCESSOR_ARCHITECTURE }
    $a = switch ($arch) {
        'AMD64' { 'x86_64' }
        'ARM64' { 'aarch64' }
        default { throw "rsc install: unsupported architecture: $arch" }
    }
    $target = "$a-pc-windows-msvc"

    $tag = $env:RSC_VERSION
    if (-not $tag) {
        $tag = @(Invoke-RestMethod 'https://api.github.com/repos/sbharga/rsc/releases?per_page=1')[0].tag_name
    }
    if (-not $tag) { throw "rsc install: couldn't find a release" }
    if (-not $tag.StartsWith('v')) { $tag = "v$tag" }

    $asset = "rsc-$target.zip"
    $url = "https://github.com/sbharga/rsc/releases/download/$tag/$asset"
    $tmp = Join-Path ([IO.Path]::GetTempPath()) ("rsc-" + [Guid]::NewGuid())
    New-Item -ItemType Directory -Force $tmp | Out-Null
    try {
        $zip = Join-Path $tmp $asset
        Write-Host "Downloading rsc $tag ($target)..."
        try { Invoke-WebRequest -UseBasicParsing $url -OutFile $zip }
        catch { throw "rsc install: download failed: $url" }
        try { Invoke-WebRequest -UseBasicParsing "$url.sha256" -OutFile "$zip.sha256" }
        catch { throw "rsc install: checksum download failed" }

        $want = ((Get-Content "$zip.sha256" -Raw).Trim() -split '\s+')[0]
        $got = (Get-FileHash -Algorithm SHA256 $zip).Hash
        if ($want -ne $got) { throw "rsc install: checksum mismatch" }

        if (Get-Command cosign -ErrorAction SilentlyContinue) {
            $bundle = "$zip.sigstore.json"
            $haveBundle = $true
            try { Invoke-WebRequest -UseBasicParsing "$url.sigstore.json" -OutFile $bundle }
            catch { $haveBundle = $false }
            if ($haveBundle) {
                & cosign verify-blob $zip --bundle $bundle `
                    --certificate-identity "https://github.com/sbharga/rsc/.github/workflows/release.yml@refs/tags/$tag" `
                    --certificate-oidc-issuer https://token.actions.githubusercontent.com
                if ($LASTEXITCODE -ne 0) { throw "rsc install: signature verification failed" }
                Write-Host "Verified signature with cosign"
            } else {
                Write-Host "No signature for $tag; verified SHA-256 only"
            }
        }

        Expand-Archive -Path $zip -DestinationPath $tmp -Force
        $dir = if ($env:RSC_INSTALL_DIR) { $env:RSC_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA 'Programs\rsc' }
        New-Item -ItemType Directory -Force $dir | Out-Null
        $exe = Join-Path $dir 'rsc.exe'
        $old = "$exe.old"
        if (Test-Path $old) { Remove-Item $old -Force -ErrorAction SilentlyContinue }
        # A running exe can be renamed but not overwritten; `rsc update` relies on this.
        if (Test-Path $exe) { Move-Item $exe $old -Force }
        Copy-Item (Join-Path $tmp "rsc-$target\rsc.exe") $exe

        Write-Host "Installed rsc $tag to $exe"
        if (($env:Path -split ';') -notcontains $dir) {
            Write-Host "Add $dir to your PATH, e.g. [Environment]::SetEnvironmentVariable('Path', `"$dir;`" + [Environment]::GetEnvironmentVariable('Path', 'User'), 'User')"
        }
    } finally {
        Remove-Item -Recurse -Force $tmp -ErrorAction SilentlyContinue
    }
}

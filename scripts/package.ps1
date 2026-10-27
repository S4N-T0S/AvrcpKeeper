# Builds a release binary and packages it as dist\AvrcpKeeper-<version>-windows-x64.zip
# with a matching .sha256 file.
param([switch]$SkipTests)

$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
Push-Location $root
try {
    # These would replace the flags in .cargo\config.toml (static CRT, CFG) instead of adding to them.
    if ($env:RUSTFLAGS -or $env:CARGO_ENCODED_RUSTFLAGS) {
        throw 'Unset RUSTFLAGS / CARGO_ENCODED_RUSTFLAGS; they override .cargo\config.toml'
    }

    $meta = cargo metadata --locked --no-deps --format-version 1 | ConvertFrom-Json
    if ($LASTEXITCODE -ne 0) { throw 'cargo metadata failed' }
    $version = $meta.packages[0].version
    $exe = Join-Path $meta.target_directory 'release\AvrcpKeeper.exe'

    # Keep the builder's home directory out of panic messages embedded in the binary.
    $cargoHome = if ($env:CARGO_HOME) { $env:CARGO_HOME } else { Join-Path $HOME '.cargo' }
    $remap = "target.x86_64-pc-windows-msvc.rustflags=['--remap-path-prefix=$cargoHome=cargo']"

    if (-not $SkipTests) {
        cargo test --release --locked --config $remap
        if ($LASTEXITCODE -ne 0) { throw 'Tests failed' }
    }
    cargo build --release --locked --config $remap
    if ($LASTEXITCODE -ne 0) { throw 'Build failed' }

    $name = "AvrcpKeeper-$version-windows-x64"
    $dist = Join-Path $root 'dist'
    $stage = Join-Path $dist $name
    $zip = Join-Path $dist "$name.zip"
    Remove-Item $stage, $zip, "$zip.sha256" -Recurse -Force -ErrorAction SilentlyContinue
    New-Item -ItemType Directory -Force $stage | Out-Null

    Copy-Item $exe, README.md, LICENSE -Destination $stage
    Compress-Archive -Path (Join-Path $stage '*') -DestinationPath $zip -CompressionLevel Optimal

    # "<hash>  <name>" without a trailing newline, which `sha256sum -c` accepts.
    $hash = (Get-FileHash $zip -Algorithm SHA256).Hash.ToLowerInvariant()
    "$hash  $name.zip" | Set-Content "$zip.sha256" -Encoding ascii -NoNewline

    Write-Host "Packaged $zip"
    Write-Host "SHA-256  $hash"
}
finally {
    Pop-Location
}

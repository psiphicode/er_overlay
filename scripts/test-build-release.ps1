[CmdletBinding()]
param()

$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path -Parent $PSScriptRoot
$testRoot = Join-Path $repoRoot 'build-release-test'
$outputPath = Join-Path $testRoot 'output'
$emptyOutputPath = Join-Path $testRoot 'empty-output'
$malformedAbsentOutputPath = Join-Path $testRoot 'malformed-absent-output'
$malformedEmptyOutputPath = Join-Path $testRoot 'malformed-empty-output'
$unownedOutputPath = Join-Path $testRoot 'unowned-output-with-a-long-name'
$markerJunctionTargetPath = Join-Path $testRoot 'marker-junction-target'
$junctionTargetPath = Join-Path $testRoot 'junction-target'
$reparseOutputPath = Join-Path $testRoot 'reparse-output'
$ancestorTargetPath = Join-Path $testRoot 'ancestor-target'
$ancestorLinkPath = Join-Path $testRoot 'ancestor-link'
$ancestorOutputPath = Join-Path $ancestorLinkPath 'nested-output'
$archiveFileLinkTargetPath = Join-Path $testRoot 'archive-link-target.zip'
$configuredFlagsOutputPath = Join-Path $testRoot 'configured-flags-output'
$spacedProfileOutputPath = Join-Path $testRoot 'spaced-profile-output'
$wrapperProbeSourcePath = Join-Path $testRoot 'rustc-wrapper-probe.rs'
$wrapperProbePath = Join-Path $testRoot 'rustc-wrapper-probe.exe'
$wrapperProbeMarkerPath = Join-Path $testRoot 'rustc-wrapper-probe.marker'
$ownsTestRoot = $false
$ownsReparseOutput = $false
$ownsReparseArchive = $false
$ownsArchiveFileLink = $false
$ownsAncestorLink = $false
$markerMagic = 'er-overlay-release-owner'
$markerVersion = 'version=1'
$markerSuffix = '.er-overlay-release-owner'
$distPath = Join-Path $repoRoot 'dist'
$manifestPath = [IO.Path]::GetFullPath((Join-Path $repoRoot 'Cargo.toml'))
$metadataJson = & cargo metadata --locked --no-deps --format-version 1 `
    --manifest-path $manifestPath
$metadataExitCode = $LASTEXITCODE
if ($metadataExitCode -ne 0) {
    throw "Cargo metadata failed with exit code $metadataExitCode"
}
$metadata = $metadataJson | ConvertFrom-Json
$rootPackage = @($metadata.packages | Where-Object {
    [IO.Path]::GetFullPath([string]$_.manifest_path) -eq $manifestPath
})
if ($rootPackage.Count -ne 1) {
    throw 'Cargo metadata did not contain exactly one workspace root package'
}
$archivePath = Join-Path $testRoot "er-overlay-$($rootPackage[0].version)-windows-x86_64.zip"

function Get-RelativeInventoryPath {
    param(
        [string]$Root,
        [string]$FullName
    )

    $rootPrefix = [IO.Path]::GetFullPath($Root).TrimEnd(
        [char[]]@([IO.Path]::DirectorySeparatorChar, [IO.Path]::AltDirectorySeparatorChar)
    ) + [IO.Path]::DirectorySeparatorChar
    $fullPath = [IO.Path]::GetFullPath($FullName)
    if (-not $fullPath.StartsWith($rootPrefix, [StringComparison]::OrdinalIgnoreCase)) {
        throw "Inventory path is outside its root: $FullName"
    }
    return $fullPath.Substring($rootPrefix.Length).Replace('\', '/')
}

function Get-DirectoryInventory {
    param([string]$Root)

    $files = @(Get-ChildItem -LiteralPath $Root -File -Recurse -Force | ForEach-Object {
        Get-RelativeInventoryPath -Root $Root -FullName $_.FullName
    })
    $directories = @(Get-ChildItem -LiteralPath $Root -Directory -Recurse -Force | ForEach-Object {
        Get-RelativeInventoryPath -Root $Root -FullName $_.FullName
    })
    return [PSCustomObject]@{
        Files = $files
        Directories = $directories
    }
}

function Assert-InventorySetEqual {
    param(
        [string]$Label,
        [string[]]$Expected,
        [string[]]$Actual
    )

    $missing = @($Expected | Where-Object { $Actual -cnotcontains $_ })
    $unexpected = @($Actual | Where-Object { $Expected -cnotcontains $_ })
    if ($Expected.Count -ne $Actual.Count -or $missing.Count -ne 0 -or $unexpected.Count -ne 0) {
        throw "$Label inventory mismatch. Missing: $($missing -join ', '). Unexpected: $($unexpected -join ', ')"
    }
}

function Get-ArchiveInventory {
    param([IO.Compression.ZipArchiveEntry[]]$Entries)

    $files = New-Object Collections.Generic.List[string]
    $directories = New-Object Collections.Generic.List[string]
    foreach ($entry in $Entries) {
        $normalizedName = $entry.FullName.Replace('\', '/').TrimStart('/')
        if ([string]::IsNullOrEmpty($entry.Name)) {
            $directoryName = $normalizedName.TrimEnd('/')
            if (-not [string]::IsNullOrEmpty($directoryName) -and
                -not $directories.Contains($directoryName)) {
                $directories.Add($directoryName)
            }
            continue
        }

        $files.Add($normalizedName)
        $separatorIndex = $normalizedName.LastIndexOf('/')
        while ($separatorIndex -ge 0) {
            $directoryName = $normalizedName.Substring(0, $separatorIndex)
            if (-not $directories.Contains($directoryName)) {
                $directories.Add($directoryName)
            }
            $separatorIndex = $directoryName.LastIndexOf('/')
        }
    }
    return [PSCustomObject]@{
        Files = @($files)
        Directories = @($directories)
    }
}

function Assert-PackagedLayout {
    param([string]$Path)

    $distInventory = Get-DirectoryInventory -Root $distPath
    $packageInventory = Get-DirectoryInventory -Root $Path
    $expectedFiles = @($distInventory.Files) + @('er_overlay.dll')
    Assert-InventorySetEqual -Label 'Packaged file' -Expected $expectedFiles `
        -Actual @($packageInventory.Files)
    Assert-InventorySetEqual -Label 'Packaged directory' -Expected @($distInventory.Directories) `
        -Actual @($packageInventory.Directories)
}

function Assert-NoUserProfilePathInBinary {
    param(
        [string]$Path,
        [string]$UserProfile
    )

    if ([string]::IsNullOrWhiteSpace($UserProfile)) {
        $UserProfile = $env:USERPROFILE
    }
    if ([string]::IsNullOrWhiteSpace($UserProfile)) {
        $UserProfile = [Environment]::GetFolderPath([Environment+SpecialFolder]::UserProfile)
    }
    if ([string]::IsNullOrWhiteSpace($UserProfile)) {
        throw 'Could not resolve the user profile path for release privacy validation'
    }

    $bytes = [IO.File]::ReadAllBytes($Path)
    $utf8Text = [Text.Encoding]::UTF8.GetString($bytes)
    $utf16EvenText = [Text.Encoding]::Unicode.GetString($bytes)
    $utf16OddText = if ($bytes.Length -gt 1) {
        [Text.Encoding]::Unicode.GetString($bytes, 1, $bytes.Length - 1)
    } else {
        ''
    }
    $profileVariants = @($UserProfile, $UserProfile.Replace('\', '/'))
    foreach ($profileVariant in $profileVariants) {
        if ($utf8Text.IndexOf($profileVariant, [StringComparison]::OrdinalIgnoreCase) -ge 0 -or
            $utf16EvenText.IndexOf($profileVariant, [StringComparison]::OrdinalIgnoreCase) -ge 0 -or
            $utf16OddText.IndexOf($profileVariant, [StringComparison]::OrdinalIgnoreCase) -ge 0) {
            throw "Packaged binary contains the local user profile path: $Path"
        }
    }
}

function Assert-ProfilePathFixtureRejected {
    param(
        [string]$Path,
        [byte[]]$Bytes,
        [string]$Label,
        [string]$UserProfile
    )

    [IO.File]::WriteAllBytes($Path, $Bytes)
    $rejected = $false
    try {
        Assert-NoUserProfilePathInBinary -Path $Path -UserProfile $UserProfile
    } catch {
        $rejected = $true
        if (-not $_.Exception.Message.StartsWith(
            'Packaged binary contains the local user profile path:',
            [StringComparison]::Ordinal
        )) {
            throw "Unexpected $Label fixture failure: $($_.Exception.Message)"
        }
    }
    if (-not $rejected) {
        throw "$Label profile-path fixture was not rejected"
    }
}

function Get-ProcessEnvironmentSnapshot {
    param([string[]]$Names)

    $snapshot = @{}
    foreach ($name in $Names) {
        $snapshot[$name] = [Environment]::GetEnvironmentVariable($name, 'Process')
    }
    return $snapshot
}

function Assert-ProcessEnvironmentSnapshot {
    param(
        [hashtable]$Expected,
        [string]$Label
    )

    foreach ($name in $Expected.Keys) {
        $actual = [Environment]::GetEnvironmentVariable($name, 'Process')
        if (-not [string]::Equals($actual, $Expected[$name], [StringComparison]::Ordinal)) {
            throw "$Label did not restore process environment variable $name"
        }
    }
}

function Assert-ConfiguredTargetRustFlagsPreserved {
    param([string]$Path)

    $variableName = 'CARGO_TARGET_X86_64_PC_WINDOWS_MSVC_RUSTFLAGS'
    $environmentNames = @(
        'CARGO_ENCODED_RUSTFLAGS',
        'CFLAGS',
        'CXXFLAGS',
        'RUSTC_WRAPPER',
        '_CL_',
        'ER_OVERLAY_RELEASE_REMAP_BACKSLASH',
        'ER_OVERLAY_RELEASE_REMAP_FORWARD',
        'ER_OVERLAY_RELEASE_INNER_RUSTC_WRAPPER'
    )
    $savedTargetFlags = [Environment]::GetEnvironmentVariable($variableName, 'Process')
    [Environment]::SetEnvironmentVariable(
        $variableName,
        '--er-overlay-invalid-configured-target-flag',
        'Process'
    )
    $environmentSnapshot = Get-ProcessEnvironmentSnapshot -Names $environmentNames
    try {
        $failed = $false
        try {
            & (Join-Path $PSScriptRoot 'build-release.ps1') -OutputPath $Path
        } catch {
            $failed = $true
            if (-not $_.Exception.Message.StartsWith('Cargo build failed with exit code', [StringComparison]::Ordinal)) {
                throw "Unexpected configured-target-rustflags failure: $($_.Exception.Message)"
            }
        }
        if (-not $failed) {
            throw 'Release build discarded configured target rustflags'
        }
        Assert-ProcessEnvironmentSnapshot -Expected $environmentSnapshot -Label 'Failed release build'
    } finally {
        [Environment]::SetEnvironmentVariable($variableName, $savedTargetFlags, 'Process')
    }
}

function Assert-ExistingRustcWrapperNested {
    param([string]$Path)

    $probeSource = @'
use std::env;
use std::fs;
use std::process::{self, Command};

fn main() {
    let mut arguments = env::args_os();
    let _wrapper = arguments.next();
    let rustc = arguments.next().expect("probe wrapper did not receive rustc");
    let marker = env::var_os("ER_OVERLAY_TEST_WRAPPER_MARKER")
        .expect("probe wrapper marker is missing");
    fs::write(marker, b"invoked").expect("probe wrapper could not write marker");
    let status = Command::new(rustc)
        .args(arguments)
        .status()
        .expect("probe wrapper could not start rustc");
    process::exit(status.code().unwrap_or(1));
}
'@
    [IO.File]::WriteAllText($wrapperProbeSourcePath, $probeSource, $utf8WithoutBom)
    & rustc --edition=2021 -O $wrapperProbeSourcePath -o $wrapperProbePath
    if ($LASTEXITCODE -ne 0) {
        throw "Test rustc wrapper build failed with exit code $LASTEXITCODE"
    }

    $savedRustcWrapper = $env:RUSTC_WRAPPER
    $savedMarker = $env:ER_OVERLAY_TEST_WRAPPER_MARKER
    try {
        $env:RUSTC_WRAPPER = $wrapperProbePath
        $env:ER_OVERLAY_TEST_WRAPPER_MARKER = $wrapperProbeMarkerPath
        Assert-ConfiguredTargetRustFlagsPreserved -Path $Path
        if (-not (Test-Path -LiteralPath $wrapperProbeMarkerPath -PathType Leaf)) {
            throw 'Release build did not invoke the existing rustc wrapper'
        }
        if (-not [string]::Equals($env:RUSTC_WRAPPER, $wrapperProbePath, [StringComparison]::Ordinal)) {
            throw 'Failed release build did not restore the existing rustc wrapper'
        }
    } finally {
        $env:RUSTC_WRAPPER = $savedRustcWrapper
        $env:ER_OVERLAY_TEST_WRAPPER_MARKER = $savedMarker
    }
}

function Assert-SpacedUserProfileBuild {
    param(
        [string]$Path,
        [string]$ActualUserProfile
    )

    $environmentNames = @(
        'USERPROFILE',
        'CARGO_HOME',
        'RUSTUP_HOME',
        'RUSTUP_TOOLCHAIN',
        'CARGO_ENCODED_RUSTFLAGS',
        'CXXFLAGS',
        '_CL_'
    )
    $environmentSnapshot = Get-ProcessEnvironmentSnapshot -Names $environmentNames
    $separator = [char]0x1f
    $actualProfileForward = $ActualUserProfile.Replace('\', '/')
    try {
        $env:USERPROFILE = Join-Path $testRoot 'Profile With Spaces'
        $env:CARGO_HOME = Join-Path $ActualUserProfile '.cargo'
        $env:RUSTUP_HOME = Join-Path $ActualUserProfile '.rustup'
        $env:RUSTUP_TOOLCHAIN = 'stable-x86_64-pc-windows-msvc'
        $env:CARGO_ENCODED_RUSTFLAGS = @(
            "--remap-path-prefix=$ActualUserProfile=/redacted/user",
            "--remap-path-prefix=$actualProfileForward=/redacted/user"
        ) -join $separator
        $env:CXXFLAGS = (@($environmentSnapshot.CXXFLAGS, '/DER_OVERLAY_SPACED_PROFILE_TEST=1') | Where-Object {
            -not [string]::IsNullOrWhiteSpace($_)
        }) -join ' '
        $actualNativeTrimFlag = "/d1trimfile:`"$ActualUserProfile`""
        $env:_CL_ = (@($environmentSnapshot.'_CL_', $actualNativeTrimFlag) | Where-Object {
            -not [string]::IsNullOrWhiteSpace($_)
        }) -join ' '

        & (Join-Path $PSScriptRoot 'build-release.ps1') -OutputPath $Path
        Assert-PackagedLayout -Path $Path
        Assert-NoUserProfilePathInBinary -Path (Join-Path $Path 'er_overlay.dll') `
            -UserProfile $ActualUserProfile
    } finally {
        foreach ($name in $environmentSnapshot.Keys) {
            [Environment]::SetEnvironmentVariable($name, $environmentSnapshot[$name], 'Process')
        }
    }
}

function Assert-ArchiveLayout {
    param([string]$Path)

    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        throw "Missing archive: $Path"
    }
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $archive = [IO.Compression.ZipFile]::OpenRead($Path)
    try {
        $archiveEntries = @($archive.Entries)
        $archiveInventory = Get-ArchiveInventory -Entries $archiveEntries
        $distInventory = Get-DirectoryInventory -Root $distPath
        $expectedFiles = @($distInventory.Files) + @('er_overlay.dll')
        Assert-InventorySetEqual -Label 'Archive file' -Expected $expectedFiles `
            -Actual @($archiveInventory.Files)
        Assert-InventorySetEqual -Label 'Archive directory' -Expected @($distInventory.Directories) `
            -Actual @($archiveInventory.Directories)
        if ($archiveInventory.Files | Where-Object { $_ -like 'output/*' }) {
            throw 'Archive incorrectly contains an output directory wrapper'
        }
    } finally {
        $archive.Dispose()
    }
}

function Assert-ReleaseRefused {
    param(
        [string]$Path,
        [string]$ExpectedMessage,
        [switch]$Zip
    )

    $refused = $false
    try {
        & (Join-Path $PSScriptRoot 'build-release.ps1') -OutputPath $Path -Zip:$Zip
    } catch {
        $refused = $true
        if (-not $_.Exception.Message.StartsWith($ExpectedMessage, [StringComparison]::Ordinal)) {
            throw "Unexpected refusal for $Path`: $($_.Exception.Message)"
        }
    }
    if (-not $refused) {
        throw "Release build unexpectedly accepted unsafe output: $Path"
    }
}

function Get-ShortPath {
    param([string]$Path)

    $shortPath = & $env:ComSpec /d /c "for %I in (`"$Path`") do @echo %~sI"
    if ($LASTEXITCODE -ne 0) {
        throw "Could not resolve disposable short path: $Path"
    }
    return [string]$shortPath
}

try {
    if (Test-Path -LiteralPath $testRoot) {
        throw "Test root already exists: $testRoot"
    }
    New-Item -ItemType Directory -Path $testRoot | Out-Null
    $ownsTestRoot = $true

    $fixtureProfile = 'C:\Users\Release Fixture'
    $fixtureUtf8 = [Text.Encoding]::UTF8.GetBytes("prefix$fixtureProfile suffix")
    $fixtureUtf16 = [Text.Encoding]::Unicode.GetBytes("prefix$fixtureProfile suffix")
    $fixtureOddUtf16 = [byte[]](@(0x2a) + [Text.Encoding]::Unicode.GetBytes($fixtureProfile) + @(0x00))
    Assert-ProfilePathFixtureRejected -Path (Join-Path $testRoot 'profile-utf8.bin') `
        -Bytes $fixtureUtf8 -Label 'UTF-8' -UserProfile $fixtureProfile
    Assert-ProfilePathFixtureRejected -Path (Join-Path $testRoot 'profile-utf16.bin') `
        -Bytes $fixtureUtf16 -Label 'even-aligned UTF-16LE' -UserProfile $fixtureProfile
    Assert-ProfilePathFixtureRejected -Path (Join-Path $testRoot 'profile-odd-utf16.bin') `
        -Bytes $fixtureOddUtf16 -Label 'odd-aligned UTF-16LE' -UserProfile $fixtureProfile

    $shortRepoRoot = Get-ShortPath -Path $repoRoot
    if (-not $shortRepoRoot.Equals($repoRoot, [StringComparison]::OrdinalIgnoreCase)) {
        Assert-ReleaseRefused -Path $shortRepoRoot -ExpectedMessage 'Unsafe output path:'
    } else {
        Write-Warning 'Repository Windows short-path alias is unavailable; protected alias assertion skipped'
    }

    New-Item -ItemType Directory -Path $ancestorTargetPath | Out-Null
    $ancestorSentinelPath = Join-Path $ancestorTargetPath 'sentinel.txt'
    Set-Content -LiteralPath $ancestorSentinelPath -Value 'preserve me'
    try {
        New-Item -ItemType Junction -Path $ancestorLinkPath -Target $ancestorTargetPath | Out-Null
        $ownsAncestorLink = $true
    } catch {
        Write-Warning "Disposable ancestor junction is unavailable; ancestor assertion skipped: $($_.Exception.Message)"
    }
    if ($ownsAncestorLink) {
        Assert-ReleaseRefused -Path $ancestorOutputPath -Zip -ExpectedMessage 'Unsafe reparse-point output path:'
        if (-not (Test-Path -LiteralPath $ancestorSentinelPath -PathType Leaf)) {
            throw 'Output beneath a junction ancestor modified the junction target sentinel'
        }
        if (Test-Path -LiteralPath (Join-Path $ancestorTargetPath 'nested-output')) {
            throw 'Output beneath a junction ancestor modified the junction target'
        }
    }

    New-Item -ItemType Directory -Path $unownedOutputPath | Out-Null
    $unownedSentinel = Join-Path $unownedOutputPath 'sentinel.txt'
    Set-Content -LiteralPath $unownedSentinel -Value 'preserve me'
    Assert-ReleaseRefused -Path $unownedOutputPath -ExpectedMessage 'Refusing to replace nonempty unowned output:'
    if (-not (Test-Path -LiteralPath $unownedSentinel -PathType Leaf)) {
        throw 'Nonempty unowned output was modified'
    }

    $unownedMarkerPath = "$unownedOutputPath$markerSuffix"
    $normalizedUnownedOutput = [IO.Path]::GetFullPath($unownedOutputPath)
    $validUnownedMarker = "$markerMagic`r`n$markerVersion`r`noutput=$normalizedUnownedOutput"
    $malformedMarkers = @(
        "wrong-magic`r`n$markerVersion`r`noutput=$normalizedUnownedOutput",
        "$markerMagic`r`nversion=2`r`noutput=$normalizedUnownedOutput",
        "$markerMagic`r`n$markerVersion`r`noutput=$outputPath",
        "$validUnownedMarker`r`n"
    )
    $utf8WithoutBom = New-Object Text.UTF8Encoding($false)
    foreach ($malformedMarker in $malformedMarkers) {
        [IO.File]::WriteAllText($unownedMarkerPath, $malformedMarker, $utf8WithoutBom)
        Assert-ReleaseRefused -Path $unownedOutputPath `
            -ExpectedMessage 'Refusing to overwrite invalid ownership marker:'
        if (-not (Test-Path -LiteralPath $unownedSentinel -PathType Leaf)) {
            throw 'Malformed ownership marker authorized output replacement'
        }
    }
    Remove-Item -LiteralPath $unownedMarkerPath -Force

    $malformedAbsentMarkerPath = "$malformedAbsentOutputPath$markerSuffix"
    $malformedAbsentMarker = 'preserve malformed absent marker'
    [IO.File]::WriteAllText($malformedAbsentMarkerPath, $malformedAbsentMarker, $utf8WithoutBom)
    Assert-ReleaseRefused -Path $malformedAbsentOutputPath `
        -ExpectedMessage 'Refusing to overwrite invalid ownership marker:'
    if (-not [IO.File]::ReadAllText($malformedAbsentMarkerPath).Equals(
        $malformedAbsentMarker,
        [StringComparison]::Ordinal
    )) {
        throw 'Malformed ownership marker beside absent output was modified'
    }
    if (Test-Path -LiteralPath $malformedAbsentOutputPath) {
        throw 'Absent output with malformed ownership marker was created'
    }

    New-Item -ItemType Directory -Path $malformedEmptyOutputPath | Out-Null
    $malformedEmptyMarkerPath = "$malformedEmptyOutputPath$markerSuffix"
    $malformedEmptyMarker = 'preserve malformed empty marker'
    [IO.File]::WriteAllText($malformedEmptyMarkerPath, $malformedEmptyMarker, $utf8WithoutBom)
    Assert-ReleaseRefused -Path $malformedEmptyOutputPath `
        -ExpectedMessage 'Refusing to overwrite invalid ownership marker:'
    if (-not [IO.File]::ReadAllText($malformedEmptyMarkerPath).Equals(
        $malformedEmptyMarker,
        [StringComparison]::Ordinal
    )) {
        throw 'Malformed ownership marker beside empty output was modified'
    }
    if (Get-ChildItem -LiteralPath $malformedEmptyOutputPath -Force | Select-Object -First 1) {
        throw 'Empty output with malformed ownership marker was modified'
    }

    New-Item -ItemType Directory -Path $markerJunctionTargetPath | Out-Null
    $markerJunctionCreated = $false
    try {
        New-Item -ItemType Junction -Path $unownedMarkerPath -Target $markerJunctionTargetPath | Out-Null
        $markerJunctionCreated = $true
    } catch {
        Write-Warning "Disposable marker junction is unavailable; reparse marker assertion skipped: $($_.Exception.Message)"
    }
    if ($markerJunctionCreated) {
        Assert-ReleaseRefused -Path $unownedOutputPath -ExpectedMessage 'Unsafe ownership marker path:'
        if (-not (Test-Path -LiteralPath $unownedSentinel -PathType Leaf)) {
            throw 'Reparse ownership marker authorized output replacement'
        }
        [IO.Directory]::Delete($unownedMarkerPath)
    }

    $shortUnownedOutputPath = Get-ShortPath -Path $unownedOutputPath
    if (-not $shortUnownedOutputPath.Equals($unownedOutputPath, [StringComparison]::OrdinalIgnoreCase)) {
        Assert-ReleaseRefused -Path $shortUnownedOutputPath -ExpectedMessage 'Refusing to replace nonempty unowned output:'
        if (-not (Test-Path -LiteralPath $unownedSentinel -PathType Leaf)) {
            throw 'Nonempty unowned short-path output was modified'
        }
    } else {
        Write-Warning 'Disposable Windows short-path alias is unavailable; alias refusal assertion skipped'
    }

    New-Item -ItemType Directory -Path $junctionTargetPath | Out-Null
    $junctionSentinel = Join-Path $junctionTargetPath 'sentinel.txt'
    Set-Content -LiteralPath $junctionSentinel -Value 'preserve me'
    try {
        New-Item -ItemType Junction -Path $reparseOutputPath -Target $junctionTargetPath | Out-Null
        $ownsReparseOutput = $true
    } catch {
        Write-Warning "Disposable junction is unavailable; reparse output assertion skipped: $($_.Exception.Message)"
    }
    if ($ownsReparseOutput) {
        Assert-ReleaseRefused -Path $reparseOutputPath -ExpectedMessage 'Unsafe reparse-point output path:'
        if (-not (Test-Path -LiteralPath $junctionSentinel -PathType Leaf)) {
            throw 'Reparse-point output target was modified'
        }
    }

    $releaseEnvironmentNames = @(
        'CFLAGS',
        'CXXFLAGS',
        'RUSTC_WRAPPER',
        '_CL_',
        'ER_OVERLAY_RELEASE_REMAP_BACKSLASH',
        'ER_OVERLAY_RELEASE_REMAP_FORWARD',
        'ER_OVERLAY_RELEASE_INNER_RUSTC_WRAPPER'
    )
    $releaseEnvironmentSnapshot = Get-ProcessEnvironmentSnapshot -Names $releaseEnvironmentNames
    & (Join-Path $PSScriptRoot 'build-release.ps1') -OutputPath $outputPath
    Assert-ProcessEnvironmentSnapshot -Expected $releaseEnvironmentSnapshot `
        -Label 'Successful release build'
    Assert-PackagedLayout -Path $outputPath
    Assert-NoUserProfilePathInBinary -Path (Join-Path $outputPath 'er_overlay.dll')
    Assert-ExistingRustcWrapperNested -Path $configuredFlagsOutputPath
    $actualUserProfile = $env:USERPROFILE
    if ([string]::IsNullOrWhiteSpace($actualUserProfile)) {
        $actualUserProfile = [Environment]::GetFolderPath([Environment+SpecialFolder]::UserProfile)
    }
    Assert-SpacedUserProfileBuild -Path $spacedProfileOutputPath `
        -ActualUserProfile $actualUserProfile
    $markerPath = "$outputPath$markerSuffix"
    $expectedMarker = "$markerMagic`r`n$markerVersion`r`noutput=$([IO.Path]::GetFullPath($outputPath))"
    if (-not (Test-Path -LiteralPath $markerPath -PathType Leaf)) {
        throw 'Release ownership marker was not created beside output'
    }
    if (-not [IO.File]::ReadAllText($markerPath).Equals($expectedMarker, [StringComparison]::Ordinal)) {
        throw 'Release ownership marker contents are invalid'
    }
    if (Get-ChildItem -LiteralPath $outputPath -Force | Where-Object { $_.Name -like "*$markerSuffix" }) {
        throw 'Release ownership marker was packaged inside output'
    }

    New-Item -ItemType Directory -Path $emptyOutputPath | Out-Null
    & (Join-Path $PSScriptRoot 'build-release.ps1') -OutputPath $emptyOutputPath
    Assert-PackagedLayout -Path $emptyOutputPath

    $stalePath = Join-Path $outputPath 'stale.txt'
    Set-Content -LiteralPath $stalePath -Value 'remove me'
    & (Join-Path $PSScriptRoot 'build-release.ps1') -OutputPath $outputPath
    Assert-PackagedLayout -Path $outputPath
    if (Test-Path -LiteralPath $stalePath) {
        throw 'Owned-output replacement preserved a stale file'
    }

    $trailingOutputPath = $outputPath + [IO.Path]::DirectorySeparatorChar
    & (Join-Path $PSScriptRoot 'build-release.ps1') -OutputPath $trailingOutputPath
    Assert-PackagedLayout -Path $outputPath
    if (-not (Test-Path -LiteralPath $markerPath -PathType Leaf)) {
        throw 'Trailing-separator output did not preserve the adjacent ownership marker'
    }
    if (Get-ChildItem -LiteralPath $outputPath -Force | Where-Object { $_.Name -like "*$markerSuffix" }) {
        throw 'Trailing-separator output placed its ownership marker inside output'
    }
    if (Test-Path $archivePath) { throw 'Folder-only build unexpectedly created a ZIP' }

    New-Item -ItemType Directory -Path $archivePath | Out-Null
    $collisionSentinel = Join-Path $archivePath 'sentinel.txt'
    Set-Content -LiteralPath $collisionSentinel -Value 'preserve me'
    $collisionMarkerPath = "$archivePath$markerSuffix"
    $collisionMarker = "$markerMagic`r`n$markerVersion`r`noutput=$([IO.Path]::GetFullPath($archivePath))"
    [IO.File]::WriteAllText($collisionMarkerPath, $collisionMarker, $utf8WithoutBom)
    Assert-ReleaseRefused -Path $archivePath -Zip `
        -ExpectedMessage 'Unsafe output and archive paths collide:'
    if (-not (Test-Path -LiteralPath $collisionSentinel -PathType Leaf)) {
        throw 'Output and archive path collision modified existing output'
    }
    Remove-Item -LiteralPath $collisionMarkerPath -Force
    Remove-Item -LiteralPath $archivePath -Recurse -Force

    New-Item -ItemType Directory -Path $archivePath | Out-Null
    $archiveDirectorySentinel = Join-Path $archivePath 'sentinel.txt'
    Set-Content -LiteralPath $archiveDirectorySentinel -Value 'preserve me'
    Assert-ReleaseRefused -Path $outputPath -Zip `
        -ExpectedMessage 'Unsafe archive path is not an ordinary file:'
    if (-not (Test-Path -LiteralPath $archiveDirectorySentinel -PathType Leaf)) {
        throw 'Archive directory was modified'
    }
    Remove-Item -LiteralPath $archivePath -Recurse -Force

    [IO.File]::WriteAllText($archiveFileLinkTargetPath, 'preserve me', $utf8WithoutBom)
    try {
        New-Item -ItemType SymbolicLink -Path $archivePath -Target $archiveFileLinkTargetPath | Out-Null
        $ownsArchiveFileLink = $true
    } catch {
        Write-Warning "Disposable archive file link is unavailable; file-reparse assertion skipped: $($_.Exception.Message)"
    }
    if ($ownsArchiveFileLink) {
        Assert-ReleaseRefused -Path $outputPath -Zip `
            -ExpectedMessage 'Unsafe archive path is not an ordinary file:'
        if (-not [IO.File]::ReadAllText($archiveFileLinkTargetPath).Equals(
            'preserve me',
            [StringComparison]::Ordinal
        )) {
            throw 'Archive file reparse-point target was modified'
        }
        [IO.File]::Delete($archivePath)
        $ownsArchiveFileLink = $false
    }

    $archiveAliasSentinel = Join-Path $outputPath 'archive-alias-sentinel.txt'
    Set-Content -LiteralPath $archiveAliasSentinel -Value 'preserve me'
    try {
        New-Item -ItemType Junction -Path $archivePath -Target $outputPath | Out-Null
        $ownsReparseArchive = $true
    } catch {
        Write-Warning "Disposable archive junction is unavailable; archive reparse assertion skipped: $($_.Exception.Message)"
    }
    if ($ownsReparseArchive) {
        Assert-ReleaseRefused -Path $outputPath -Zip `
            -ExpectedMessage 'Unsafe archive path is not an ordinary file:'
        if (-not (Test-Path -LiteralPath $archiveAliasSentinel -PathType Leaf)) {
            throw 'Archive reparse-point alias modified the output before refusal'
        }
        [IO.Directory]::Delete($archivePath)
        $ownsReparseArchive = $false
    }
    Remove-Item -LiteralPath $archiveAliasSentinel -Force

    & (Join-Path $PSScriptRoot 'build-release.ps1') -OutputPath $outputPath -Zip
    Assert-ArchiveLayout -Path $archivePath

    [IO.File]::WriteAllText($archivePath, 'stale archive', $utf8WithoutBom)
    & (Join-Path $PSScriptRoot 'build-release.ps1') -OutputPath $outputPath -Zip
    Assert-ArchiveLayout -Path $archivePath
} finally {
    if ($ownsAncestorLink -and (Test-Path -LiteralPath $ancestorLinkPath)) {
        [IO.Directory]::Delete($ancestorLinkPath)
    }
    if ($ownsArchiveFileLink -and (Test-Path -LiteralPath $archivePath)) {
        [IO.File]::Delete($archivePath)
    }
    if ($ownsReparseArchive -and (Test-Path -LiteralPath $archivePath)) {
        [IO.Directory]::Delete($archivePath)
    }
    if ($ownsReparseOutput -and (Test-Path -LiteralPath $reparseOutputPath)) {
        [IO.Directory]::Delete($reparseOutputPath)
    }
    if ($ownsTestRoot -and (Test-Path -LiteralPath $testRoot)) {
        Remove-Item -LiteralPath $testRoot -Recurse -Force
    }
}

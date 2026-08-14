[CmdletBinding()]
param(
    [string]$OutputPath,
    [switch]$Zip
)

$ErrorActionPreference = 'Stop'

if (-not ('ErOverlayRelease.NativePath' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
using System.Text;
using Microsoft.Win32.SafeHandles;

namespace ErOverlayRelease
{
    public static class NativePath
    {
        [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        public static extern SafeFileHandle CreateFile(
            string fileName,
            uint desiredAccess,
            uint shareMode,
            IntPtr securityAttributes,
            uint creationDisposition,
            uint flagsAndAttributes,
            IntPtr templateFile);

        [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        public static extern uint GetFinalPathNameByHandle(
            SafeFileHandle file,
            StringBuilder filePath,
            uint filePathLength,
            uint flags);
    }
}
'@
}

function Get-NormalizedFullPath {
    param([string]$Path)

    $fullPath = [IO.Path]::GetFullPath($Path)
    $root = [IO.Path]::GetPathRoot($fullPath)
    if (-not [string]::Equals($fullPath, $root, [StringComparison]::OrdinalIgnoreCase)) {
        $trimCharacters = [char[]]@(
            [IO.Path]::DirectorySeparatorChar,
            [IO.Path]::AltDirectorySeparatorChar
        )
        $fullPath = $fullPath.TrimEnd($trimCharacters)
    }
    return $fullPath
}

function Assert-NoReparsePointInExistingChain {
    param(
        [string]$Path,
        [string]$ErrorPrefix
    )

    $probe = Get-NormalizedFullPath -Path $Path
    while (-not [string]::IsNullOrEmpty($probe)) {
        try {
            $attributes = [IO.File]::GetAttributes($probe)
            if (($attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
                throw "$ErrorPrefix $Path"
            }
        } catch [IO.FileNotFoundException] {
        } catch [IO.DirectoryNotFoundException] {
        }

        $parent = Split-Path -Parent $probe
        if ([string]::IsNullOrEmpty($parent) -or
            [string]::Equals($parent, $probe, [StringComparison]::OrdinalIgnoreCase)) {
            break
        }
        $probe = $parent
    }
}

function Get-FinalPathName {
    param([string]$Path)

    $handle = [ErOverlayRelease.NativePath]::CreateFile(
        $Path,
        0,
        7,
        [IntPtr]::Zero,
        3,
        0x02000000,
        [IntPtr]::Zero
    )
    if ($handle.IsInvalid) {
        $errorCode = [Runtime.InteropServices.Marshal]::GetLastWin32Error()
        $handle.Dispose()
        throw "Could not canonicalize existing path '$Path': $((New-Object ComponentModel.Win32Exception($errorCode)).Message)"
    }

    try {
        $capacity = 512
        while ($true) {
            $buffer = New-Object Text.StringBuilder($capacity)
            $length = [ErOverlayRelease.NativePath]::GetFinalPathNameByHandle(
                $handle,
                $buffer,
                [uint32]$buffer.Capacity,
                0
            )
            if ($length -eq 0) {
                $errorCode = [Runtime.InteropServices.Marshal]::GetLastWin32Error()
                throw "Could not canonicalize existing path '$Path': $((New-Object ComponentModel.Win32Exception($errorCode)).Message)"
            }
            if ($length -lt $buffer.Capacity) {
                $finalPath = $buffer.ToString()
                if ($finalPath.StartsWith('\\?\UNC\', [StringComparison]::OrdinalIgnoreCase)) {
                    $finalPath = '\\' + $finalPath.Substring(8)
                } elseif ($finalPath.StartsWith('\\?\', [StringComparison]::OrdinalIgnoreCase)) {
                    $finalPath = $finalPath.Substring(4)
                }
                return Get-NormalizedFullPath -Path $finalPath
            }
            $capacity = [int]$length + 1
        }
    } finally {
        $handle.Dispose()
    }
}

function Resolve-SafeCanonicalPath {
    param(
        [string]$Path,
        [string]$ReparseErrorPrefix
    )

    $fullPath = Get-NormalizedFullPath -Path $Path
    Assert-NoReparsePointInExistingChain -Path $fullPath -ErrorPrefix $ReparseErrorPrefix

    $remainingNames = New-Object Collections.Generic.List[string]
    $existingPath = $fullPath
    while (-not (Test-Path -LiteralPath $existingPath)) {
        $leafName = Split-Path -Leaf $existingPath
        if ([string]::IsNullOrEmpty($leafName)) {
            throw "Could not find an existing ancestor for path: $Path"
        }
        $remainingNames.Add($leafName)
        $existingPath = Split-Path -Parent $existingPath
    }

    $canonicalPath = Get-FinalPathName -Path $existingPath
    for ($index = $remainingNames.Count - 1; $index -ge 0; $index--) {
        $canonicalPath = Join-Path $canonicalPath $remainingNames[$index]
    }
    return Get-NormalizedFullPath -Path $canonicalPath
}

function Assert-CanonicalPathIdentity {
    param(
        [string]$Path,
        [string]$ExpectedCanonicalPath,
        [string]$ReparseErrorPrefix,
        [string]$IdentityErrorPrefix
    )

    $actualCanonicalPath = Resolve-SafeCanonicalPath -Path $Path `
        -ReparseErrorPrefix $ReparseErrorPrefix
    if (-not [string]::Equals(
        $actualCanonicalPath,
        $ExpectedCanonicalPath,
        [StringComparison]::OrdinalIgnoreCase
    )) {
        throw "$IdentityErrorPrefix $Path"
    }
}

$repoRoot = Resolve-SafeCanonicalPath -Path (Split-Path -Parent $PSScriptRoot) `
    -ReparseErrorPrefix 'Unsafe repository path:'
if ([string]::IsNullOrWhiteSpace($OutputPath)) {
    $OutputPath = Join-Path $repoRoot 'output'
}
$outputCandidate = if ([IO.Path]::IsPathRooted($OutputPath)) {
    $OutputPath
} else {
    Join-Path $repoRoot $OutputPath
}
$resolvedOutput = Resolve-SafeCanonicalPath -Path $outputCandidate `
    -ReparseErrorPrefix 'Unsafe reparse-point output path:'
$filesystemRoot = [IO.Path]::GetPathRoot($resolvedOutput)
if ([string]::Equals(
    $resolvedOutput,
    $filesystemRoot,
    [StringComparison]::OrdinalIgnoreCase
)) {
    throw "Unsafe output path: $resolvedOutput"
}
$markerMagic = 'er-overlay-release-owner'
$markerVersion = 'version=1'
$markerPath = Resolve-SafeCanonicalPath -Path "$resolvedOutput.er-overlay-release-owner" `
    -ReparseErrorPrefix 'Unsafe ownership marker path:'
$expectedMarker = "$markerMagic`r`n$markerVersion`r`noutput=$resolvedOutput"

function Test-IsWithinPath {
    param([string]$Candidate, [string]$Parent)

    $separator = [IO.Path]::DirectorySeparatorChar
    $trimCharacters = [char[]]@($separator, [IO.Path]::AltDirectorySeparatorChar)
    $parentPrefix = $Parent.TrimEnd($trimCharacters) + $separator
    return $Candidate.StartsWith($parentPrefix, [StringComparison]::OrdinalIgnoreCase)
}

function Assert-SafeMarkerPath {
    param([string]$Path)

    Assert-CanonicalPathIdentity -Path $Path -ExpectedCanonicalPath $Path `
        -ReparseErrorPrefix 'Unsafe ownership marker path:' `
        -IdentityErrorPrefix 'Ownership marker path identity changed:'
    if (-not (Test-Path -LiteralPath $Path)) {
        return
    }
    $markerItem = Get-Item -LiteralPath $Path -Force
    if ($markerItem.PSIsContainer -or
        (($markerItem.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0)) {
        throw "Unsafe ownership marker path: $Path"
    }
}

function Test-ValidOwnershipMarker {
    param(
        [string]$Path,
        [string]$ExpectedContent
    )

    Assert-SafeMarkerPath -Path $Path
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        return $false
    }
    $actualContent = [IO.File]::ReadAllText($Path)
    return $actualContent.Equals($ExpectedContent, [StringComparison]::Ordinal)
}

function Get-OutputState {
    param(
        [string]$Path,
        [string]$OwnershipMarkerPath,
        [string]$ExpectedMarkerContent
    )

    Assert-CanonicalPathIdentity -Path $Path -ExpectedCanonicalPath $Path `
        -ReparseErrorPrefix 'Unsafe reparse-point output path:' `
        -IdentityErrorPrefix 'Output path identity changed:'
    Assert-SafeMarkerPath -Path $OwnershipMarkerPath
    $markerIsValid = $false
    if (Test-Path -LiteralPath $OwnershipMarkerPath) {
        $markerIsValid = Test-ValidOwnershipMarker -Path $OwnershipMarkerPath `
            -ExpectedContent $ExpectedMarkerContent
        if (-not $markerIsValid) {
            throw "Refusing to overwrite invalid ownership marker: $OwnershipMarkerPath"
        }
    }
    if (-not (Test-Path -LiteralPath $Path)) {
        return 'Absent'
    }

    $outputItem = Get-Item -LiteralPath $Path -Force
    if (-not $outputItem.PSIsContainer) {
        throw "Unsafe output path is not a directory: $Path"
    }
    if (($outputItem.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
        throw "Unsafe reparse-point output path: $Path"
    }
    if (-not (Get-ChildItem -LiteralPath $Path -Force | Select-Object -First 1)) {
        return 'Empty'
    }
    if (-not $markerIsValid) {
        throw "Refusing to replace nonempty unowned output: $Path"
    }
    return 'OwnedNonempty'
}

function Get-ArchiveState {
    param([string]$Path)

    Assert-CanonicalPathIdentity -Path $Path -ExpectedCanonicalPath $Path `
        -ReparseErrorPrefix 'Unsafe archive path is not an ordinary file:' `
        -IdentityErrorPrefix 'Archive path identity changed:'
    if (-not (Test-Path -LiteralPath $Path)) {
        return 'Absent'
    }
    $archiveItem = Get-Item -LiteralPath $Path -Force
    if ($archiveItem.PSIsContainer -or
        (($archiveItem.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0)) {
        throw "Unsafe archive path is not an ordinary file: $Path"
    }
    return 'OrdinaryFile'
}

function Get-WorkspaceRootPackageVersion {
    param([string]$RepositoryRoot)

    $manifestPath = [IO.Path]::GetFullPath((Join-Path $RepositoryRoot 'Cargo.toml'))
    $metadataJson = & cargo metadata --locked --no-deps --format-version 1 `
        --manifest-path $manifestPath
    $metadataExitCode = $LASTEXITCODE
    if ($metadataExitCode -ne 0) {
        throw "Cargo metadata failed with exit code $metadataExitCode"
    }
    try {
        $metadata = (@($metadataJson) -join [Environment]::NewLine) | ConvertFrom-Json
    } catch {
        throw "Cargo metadata returned invalid JSON: $($_.Exception.Message)"
    }

    $workspaceManifestPath = [IO.Path]::GetFullPath(
        (Join-Path ([string]$metadata.workspace_root) 'Cargo.toml')
    )
    $rootPackages = @($metadata.packages | Where-Object {
        [string]::Equals(
            [IO.Path]::GetFullPath([string]$_.manifest_path),
            $workspaceManifestPath,
            [StringComparison]::OrdinalIgnoreCase
        )
    })
    if ($rootPackages.Count -ne 1) {
        throw 'Cargo metadata did not contain exactly one workspace root package'
    }
    $version = [string]$rootPackages[0].version
    if ([string]::IsNullOrWhiteSpace($version)) {
        throw 'Cargo metadata returned an empty workspace root package version'
    }
    return $version
}

function Assert-ArchiveLayout {
    param([string]$Path)

    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $archive = [IO.Compression.ZipFile]::OpenRead($Path)
    try {
        $archiveEntries = @($archive.Entries)
        $entryNames = @($archiveEntries | ForEach-Object {
            $_.FullName.Replace('\', '/')
        })
        if ('er_overlay.dll' -notin $entryNames) {
            throw "Archive is missing er_overlay.dll: $Path"
        }
        if ('overlay_config.toml' -notin $entryNames) {
            throw "Archive is missing overlay_config.toml: $Path"
        }
        if (-not ($archiveEntries | Where-Object {
            $normalizedName = $_.FullName.Replace('\', '/')
            $normalizedName.StartsWith('data/') -and -not [string]::IsNullOrEmpty($_.Name)
        } | Select-Object -First 1)) {
            throw "Archive data directory contains no files: $Path"
        }
    } finally {
        $archive.Dispose()
    }
}

if (Test-IsWithinPath $repoRoot $resolvedOutput) {
    throw "Unsafe output path: $resolvedOutput"
}

$protectedPaths = @(
    $repoRoot,
    (Join-Path $repoRoot 'dist'),
    (Join-Path $repoRoot 'target')
)
foreach ($protectedPath in $protectedPaths) {
    $protectedPath = Resolve-SafeCanonicalPath -Path $protectedPath `
        -ReparseErrorPrefix 'Unsafe protected path:'
    if ([string]::Equals(
        $resolvedOutput,
        $protectedPath,
        [StringComparison]::OrdinalIgnoreCase
    ) -or
        (-not [string]::Equals(
            $protectedPath,
            $repoRoot,
            [StringComparison]::OrdinalIgnoreCase
        ) -and (Test-IsWithinPath $resolvedOutput $protectedPath))) {
        throw "Unsafe output path: $resolvedOutput"
    }
}

Get-OutputState -Path $resolvedOutput -OwnershipMarkerPath $markerPath `
    -ExpectedMarkerContent $expectedMarker | Out-Null

$archiveName = $null
$archivePath = $null
if ($Zip) {
    $packageVersion = Get-WorkspaceRootPackageVersion -RepositoryRoot $repoRoot
    $archiveName = "er-overlay-$packageVersion-windows-x86_64.zip"
    $archivePath = Resolve-SafeCanonicalPath `
        -Path (Join-Path (Split-Path -Parent $resolvedOutput) $archiveName) `
        -ReparseErrorPrefix 'Unsafe archive path is not an ordinary file:'
    if ([string]::Equals(
        $resolvedOutput,
        $archivePath,
        [StringComparison]::OrdinalIgnoreCase
    )) {
        throw "Unsafe output and archive paths collide: $resolvedOutput"
    }
    Get-ArchiveState -Path $archivePath | Out-Null
}

Push-Location $repoRoot
try {
    & cargo build --locked --release --target x86_64-pc-windows-msvc
    if ($LASTEXITCODE -ne 0) {
        throw "Cargo build failed with exit code $LASTEXITCODE"
    }
} finally {
    Pop-Location
}

$dllPath = Join-Path $repoRoot 'target\x86_64-pc-windows-msvc\release\er_overlay.dll'
$distPath = Join-Path $repoRoot 'dist'
if (-not (Test-Path -LiteralPath $dllPath -PathType Leaf)) {
    throw "Built DLL was not found: $dllPath"
}
if (-not (Test-Path -LiteralPath $distPath -PathType Container)) {
    throw "Distribution directory was not found: $distPath"
}

$outputParent = Resolve-SafeCanonicalPath -Path (Split-Path -Parent $resolvedOutput) `
    -ReparseErrorPrefix 'Unsafe reparse-point output path:'
if (-not (Test-Path -LiteralPath $outputParent -PathType Container)) {
    New-Item -ItemType Directory -Path $outputParent -Force | Out-Null
}
Assert-CanonicalPathIdentity -Path $outputParent -ExpectedCanonicalPath $outputParent `
    -ReparseErrorPrefix 'Unsafe reparse-point output path:' `
    -IdentityErrorPrefix 'Output parent path identity changed:'
$outputName = Split-Path -Leaf $resolvedOutput
$stagingPath = Resolve-SafeCanonicalPath `
    -Path (Join-Path $outputParent ".$outputName.er-overlay-staging-$([Guid]::NewGuid().ToString('N'))") `
    -ReparseErrorPrefix 'Unsafe release staging path:'
$ownsStagingPath = $false

try {
    New-Item -ItemType Directory -Path $stagingPath | Out-Null
    $ownsStagingPath = $true
    Assert-CanonicalPathIdentity -Path $stagingPath -ExpectedCanonicalPath $stagingPath `
        -ReparseErrorPrefix 'Unsafe release staging path:' `
        -IdentityErrorPrefix 'Release staging path identity changed:'
    Copy-Item -Path (Join-Path $distPath '*') -Destination $stagingPath -Recurse -Force
    Copy-Item -LiteralPath $dllPath -Destination (Join-Path $stagingPath 'er_overlay.dll') -Force

    $stagedDllPath = Join-Path $stagingPath 'er_overlay.dll'
    $stagedConfigPath = Join-Path $stagingPath 'overlay_config.toml'
    $stagedDataPath = Join-Path $stagingPath 'data'
    if (-not (Test-Path -LiteralPath $stagedDllPath -PathType Leaf)) {
        throw "Staged DLL was not found: $stagedDllPath"
    }
    if (-not (Test-Path -LiteralPath $stagedConfigPath -PathType Leaf)) {
        throw "Staged configuration was not found: $stagedConfigPath"
    }
    if (-not (Test-Path -LiteralPath $stagedDataPath -PathType Container) -or
        -not (Get-ChildItem -LiteralPath $stagedDataPath -File -Recurse | Select-Object -First 1)) {
        throw "Staged data directory contains no files: $stagedDataPath"
    }

    $outputState = Get-OutputState -Path $resolvedOutput -OwnershipMarkerPath $markerPath `
        -ExpectedMarkerContent $expectedMarker
    Assert-CanonicalPathIdentity -Path $stagingPath -ExpectedCanonicalPath $stagingPath `
        -ReparseErrorPrefix 'Unsafe release staging path:' `
        -IdentityErrorPrefix 'Release staging path identity changed:'
    if ($outputState -eq 'Empty') {
        Remove-Item -LiteralPath $resolvedOutput -Force
    } elseif ($outputState -eq 'OwnedNonempty') {
        Remove-Item -LiteralPath $resolvedOutput -Recurse -Force
    }

    Move-Item -LiteralPath $stagingPath -Destination $resolvedOutput
    $ownsStagingPath = $false
    Assert-CanonicalPathIdentity -Path $resolvedOutput -ExpectedCanonicalPath $resolvedOutput `
        -ReparseErrorPrefix 'Unsafe reparse-point output path:' `
        -IdentityErrorPrefix 'Output path identity changed:'
    Assert-SafeMarkerPath -Path $markerPath
    $utf8WithoutBom = New-Object Text.UTF8Encoding($false)
    [IO.File]::WriteAllText($markerPath, $expectedMarker, $utf8WithoutBom)
} finally {
    if ($ownsStagingPath -and (Test-Path -LiteralPath $stagingPath)) {
        try {
            Assert-CanonicalPathIdentity -Path $stagingPath -ExpectedCanonicalPath $stagingPath `
                -ReparseErrorPrefix 'Unsafe release staging path:' `
                -IdentityErrorPrefix 'Release staging path identity changed:'
            Remove-Item -LiteralPath $stagingPath -Recurse -Force
        } catch {
            Write-Warning "Refusing to clean unsafe release staging path '$stagingPath': $($_.Exception.Message)"
        }
    }
}

Write-Host "Release folder: $resolvedOutput"

if ($Zip) {
    $temporaryArchiveName = ".$archiveName.er-overlay-staging-$([Guid]::NewGuid().ToString('N')).zip"
    $temporaryArchivePath = Resolve-SafeCanonicalPath `
        -Path (Join-Path $outputParent $temporaryArchiveName) `
        -ReparseErrorPrefix 'Unsafe temporary archive path:'
    $ownsTemporaryArchive = $true
    $archiveBackupPath = $null
    $ownsArchiveBackup = $false
    try {
        Assert-CanonicalPathIdentity -Path $resolvedOutput -ExpectedCanonicalPath $resolvedOutput `
            -ReparseErrorPrefix 'Unsafe reparse-point output path:' `
            -IdentityErrorPrefix 'Output path identity changed:'
        Assert-CanonicalPathIdentity -Path $temporaryArchivePath `
            -ExpectedCanonicalPath $temporaryArchivePath `
            -ReparseErrorPrefix 'Unsafe temporary archive path:' `
            -IdentityErrorPrefix 'Temporary archive path identity changed:'
        Compress-Archive -Path (Join-Path $resolvedOutput '*') `
            -DestinationPath $temporaryArchivePath -CompressionLevel Optimal
        Assert-CanonicalPathIdentity -Path $temporaryArchivePath `
            -ExpectedCanonicalPath $temporaryArchivePath `
            -ReparseErrorPrefix 'Unsafe temporary archive path:' `
            -IdentityErrorPrefix 'Temporary archive path identity changed:'
        Assert-ArchiveLayout -Path $temporaryArchivePath

        $archiveState = Get-ArchiveState -Path $archivePath
        if ($archiveState -eq 'OrdinaryFile') {
            $archiveBackupName = ".$archiveName.er-overlay-backup-$([Guid]::NewGuid().ToString('N'))"
            $archiveBackupPath = Resolve-SafeCanonicalPath `
                -Path (Join-Path $outputParent $archiveBackupName) `
                -ReparseErrorPrefix 'Unsafe archive backup path:'
            if (Test-Path -LiteralPath $archiveBackupPath) {
                throw "Archive backup path unexpectedly exists: $archiveBackupPath"
            }
            $ownsArchiveBackup = $true
            Assert-CanonicalPathIdentity -Path $archivePath -ExpectedCanonicalPath $archivePath `
                -ReparseErrorPrefix 'Unsafe archive path is not an ordinary file:' `
                -IdentityErrorPrefix 'Archive path identity changed:'
            Assert-CanonicalPathIdentity -Path $temporaryArchivePath `
                -ExpectedCanonicalPath $temporaryArchivePath `
                -ReparseErrorPrefix 'Unsafe temporary archive path:' `
                -IdentityErrorPrefix 'Temporary archive path identity changed:'
            Assert-CanonicalPathIdentity -Path $archiveBackupPath `
                -ExpectedCanonicalPath $archiveBackupPath `
                -ReparseErrorPrefix 'Unsafe archive backup path:' `
                -IdentityErrorPrefix 'Archive backup path identity changed:'
            [IO.File]::Replace($temporaryArchivePath, $archivePath, $archiveBackupPath)
            $ownsTemporaryArchive = $false
            Remove-Item -LiteralPath $archiveBackupPath -Force
            $ownsArchiveBackup = $false
        } else {
            Assert-CanonicalPathIdentity -Path $archivePath -ExpectedCanonicalPath $archivePath `
                -ReparseErrorPrefix 'Unsafe archive path is not an ordinary file:' `
                -IdentityErrorPrefix 'Archive path identity changed:'
            Assert-CanonicalPathIdentity -Path $temporaryArchivePath `
                -ExpectedCanonicalPath $temporaryArchivePath `
                -ReparseErrorPrefix 'Unsafe temporary archive path:' `
                -IdentityErrorPrefix 'Temporary archive path identity changed:'
            [IO.File]::Move($temporaryArchivePath, $archivePath)
            $ownsTemporaryArchive = $false
        }
    } finally {
        if ($ownsTemporaryArchive -and (Test-Path -LiteralPath $temporaryArchivePath)) {
            try {
                Assert-CanonicalPathIdentity -Path $temporaryArchivePath `
                    -ExpectedCanonicalPath $temporaryArchivePath `
                    -ReparseErrorPrefix 'Unsafe temporary archive path:' `
                    -IdentityErrorPrefix 'Temporary archive path identity changed:'
                $temporaryItem = Get-Item -LiteralPath $temporaryArchivePath -Force
                if (-not $temporaryItem.PSIsContainer) {
                    Remove-Item -LiteralPath $temporaryArchivePath -Force
                }
            } catch {
                Write-Warning "Refusing to clean unsafe temporary archive '$temporaryArchivePath': $($_.Exception.Message)"
            }
        }
        if ($ownsArchiveBackup -and (Test-Path -LiteralPath $archiveBackupPath)) {
            try {
                Assert-CanonicalPathIdentity -Path $archiveBackupPath `
                    -ExpectedCanonicalPath $archiveBackupPath `
                    -ReparseErrorPrefix 'Unsafe archive backup path:' `
                    -IdentityErrorPrefix 'Archive backup path identity changed:'
                $backupItem = Get-Item -LiteralPath $archiveBackupPath -Force
                if (-not $backupItem.PSIsContainer) {
                    Remove-Item -LiteralPath $archiveBackupPath -Force
                }
            } catch {
                Write-Warning "Refusing to clean unsafe archive backup '$archiveBackupPath': $($_.Exception.Message)"
            }
        }
    }

    Write-Host "Release archive: $archivePath"
}

<#
.SYNOPSIS
  Checks that the Windows build carries the MailCraft icon where the shell looks for it.

      packaging/windows/check-icon.ps1 -Exe <mailcraft.exe> [-Msi <mailcraft-....msi>]

  1. mailcraft.exe has an icon group resource (RT_GROUP_ICON). Explorer, the taskbar and pinned shortcuts
     use it; build.rs embeds it when a resource compiler is found.
  2. The MSI's Icon table names its icon with an .ico extension. Windows Installer extracts shortcut icons
     under that name and reads their format from the extension; without it the Start menu shows a blank icon.

  Exits 1 with a message when either check fails.
#>
param(
    [Parameter(Mandatory = $true)][string]$Exe,
    [string]$Msi
)
$ErrorActionPreference = "Stop"

Add-Type -Namespace MailCraft -Name Res -MemberDefinition @"
[DllImport("kernel32.dll", SetLastError = true, CharSet = CharSet.Unicode)]
public static extern IntPtr LoadLibraryEx(string path, IntPtr file, uint flags);
[DllImport("kernel32.dll", SetLastError = true)]
public static extern bool FreeLibrary(IntPtr module);
[DllImport("kernel32.dll", SetLastError = true)]
public static extern bool EnumResourceNames(IntPtr module, IntPtr type, EnumResNameProc callback, IntPtr param);
public delegate bool EnumResNameProc(IntPtr module, IntPtr type, IntPtr name, IntPtr param);
"@

$failed = $false
$path = (Resolve-Path $Exe).Path
# LOAD_LIBRARY_AS_DATAFILE | LOAD_LIBRARY_AS_IMAGE_RESOURCE: read resources without running the file.
$module = [MailCraft.Res]::LoadLibraryEx($path, [IntPtr]::Zero, 0x22)
if ($module -eq [IntPtr]::Zero) {
    Write-Host "check-icon: cannot open $path"
    exit 1
}
$script:groups = 0
$callback = [MailCraft.Res+EnumResNameProc] { param($m, $t, $n, $p) $script:groups++; return $true }
# RT_GROUP_ICON = 14.
[void][MailCraft.Res]::EnumResourceNames($module, [IntPtr]14, $callback, [IntPtr]::Zero)
[void][MailCraft.Res]::FreeLibrary($module)
if ($script:groups -lt 1) {
    Write-Host "check-icon: $path has no icon resource (was a resource compiler found by build.rs?)"
    $failed = $true
} else {
    Write-Host "check-icon: $path has $($script:groups) icon group resource(s)"
}

if ($Msi) {
    $installer = New-Object -ComObject WindowsInstaller.Installer
    $db = $installer.GetType().InvokeMember("OpenDatabase", "InvokeMethod", $null, $installer,
        @((Resolve-Path $Msi).Path, 0))
    $view = $db.GetType().InvokeMember("OpenView", "InvokeMethod", $null, $db, @("SELECT Name FROM Icon"))
    [void]$view.GetType().InvokeMember("Execute", "InvokeMethod", $null, $view, $null)
    $names = @()
    while ($true) {
        $record = $view.GetType().InvokeMember("Fetch", "InvokeMethod", $null, $view, $null)
        if ($null -eq $record) { break }
        $names += $record.GetType().InvokeMember("StringData", "GetProperty", $null, $record, @(1))
    }
    [void]$view.GetType().InvokeMember("Close", "InvokeMethod", $null, $view, $null)
    if ($names.Count -lt 1) {
        Write-Host "check-icon: $Msi has no Icon table entries"
        $failed = $true
    } elseif (@($names | Where-Object { $_ -notmatch '\.(ico|exe)$' }).Count -gt 0) {
        Write-Host "check-icon: MSI icon names without an .ico/.exe extension: $($names -join ', ')"
        $failed = $true
    } else {
        Write-Host "check-icon: MSI icons: $($names -join ', ')"
    }
}

if ($failed) { exit 1 }

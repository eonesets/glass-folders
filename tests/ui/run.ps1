# Кратко: запускает UI-сценарии без настоящей мыши. Делает временный «рабочий стол» и свои настройки,
# прогоняет каждый scenarios/*.gfs через glass-folders.exe (GF_SCRIPT), печатает PASS/FAIL. Картинки в tests/ui/out/<сценарий>/.
#
# Usage: powershell -File tests/ui/run.ps1 [name ...]   (no names = all scenarios)
# Needs: app/target/release/glass-folders.exe (cargo build --release in app/), or env GF_EXE = other build.
# Safe while the user works: windows invisible + click-through, no focus steal, real cursor never touched,
# demo desktop in %TEMP% (real Desktop and real settings untouched), autostart in a test registry key.
# Scenario lines '#fixture big' (+ folder Big, 60 items), '#fixture extdrop' (+ loose desktop files),
# '#fixture bottom-tile' (places Games 2x2 tile near bottom of physical work area) extend the fixture.

param([Parameter(ValueFromRemainingArguments = $true)][string[]]$Names)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $MyInvocation.MyCommand.Path
$exe = if ($env:GF_EXE) { Resolve-Path $env:GF_EXE } else { Join-Path $root '..\..\app\target\release\glass-folders.exe' | Resolve-Path }
$outAll = Join-Path $root 'out'
$scen = Get-ChildItem (Join-Path $root 'scenarios') -Filter *.gfs | Where-Object { -not $Names -or $Names -contains $_.BaseName }

# Physical work area (same coordinates as win::work_area() in DPI-aware glass-folders.exe).
if (-not ('GfWinArea' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class GfWinArea {
    [StructLayout(LayoutKind.Sequential)]
    public struct RECT { public int Left, Top, Right, Bottom; }
    [DllImport("user32.dll")] static extern bool SetProcessDpiAwarenessContext(IntPtr ctx);
    [DllImport("user32.dll")] static extern bool SystemParametersInfoW(uint a, uint b, out RECT r, uint f);
    [DllImport("user32.dll")] static extern uint GetDpiForSystem();
    public static int[] Get() {
        try { SetProcessDpiAwarenessContext(new IntPtr(-4)); } catch {}
        RECT r;
        SystemParametersInfoW(0x0030, 0, out r, 0);
        uint dpi = 96;
        try { dpi = GetDpiForSystem(); } catch {}
        return new int[] { r.Left, r.Top, r.Right, r.Bottom, (int)dpi };
    }
}
'@
}

# Real user Run\GlassFolders snapshot + restore guard: tests must NEVER mutate the user's real autostart key.
$realRunKey = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run'
function Get-RealAutostart {
    try {
        $p = Get-ItemProperty -Path $realRunKey -Name 'GlassFolders' -ErrorAction Stop
        return @{ Exists = $true; Value = [string]$p.GlassFolders }
    } catch {
        return @{ Exists = $false; Value = $null }
    }
}
function Restore-RealAutostart($snap) {
    if ($snap.Exists) {
        Set-ItemProperty -Path $realRunKey -Name 'GlassFolders' -Value $snap.Value -Type String
    } else {
        Remove-ItemProperty -Path $realRunKey -Name 'GlassFolders' -ErrorAction SilentlyContinue
    }
}

# Fresh demo desktop: folder Work = groups Docs, Tools + loose files (Misc group).
function New-Fixture($dir) {
    if (Test-Path $dir) { Remove-Item $dir -Recurse -Force }
    $w = Join-Path $dir 'Work'
    foreach ($d in 'Docs', 'Tools') { New-Item -ItemType Directory -Force (Join-Path $w $d) | Out-Null }
    # items = shortcut types only (model::is_item); .bat that do nothing, in case one is ever launched
    foreach ($f in 'Docs\alpha.bat', 'Docs\beta.bat', 'Tools\gamma.bat', 'notes.bat') { 'rem test' | Set-Content (Join-Path $w $f) }
    # second folder: drop target for shortcuts dragged out of Work
    $m = Join-Path $dir 'Games'
    New-Item -ItemType Directory -Force $m | Out-Null
    'rem test' | Set-Content (Join-Path $m 'chess.bat')
}

# Optional fixture parts, by '#fixture NAME' lines in the scenario.
function Add-Fixture($dir, $names) {
    if ($names -contains 'big') {
        # Big = 3 groups x 15 + 15 loose = 60 items
        $b = Join-Path $dir 'Big'
        foreach ($gi in 1..3) {
            $gd = Join-Path $b "Group$gi"
            New-Item -ItemType Directory -Force $gd | Out-Null
            foreach ($i in 1..15) { 'rem test' | Set-Content (Join-Path $gd ('g{0}item{1:D2}.bat' -f $gi, $i)) }
        }
        foreach ($i in 1..15) { 'rem test' | Set-Content (Join-Path $b ('loose{0:D2}.bat' -f $i)) }
    }
    if ($names -contains 'extdrop') {
        # loose desktop files = things dragged in from outside
        foreach ($f in 'loose.bat', 'copyme.bat') { 'rem test' | Set-Content (Join-Path $dir $f) }
    }
    if ($names -contains 'bottom-tile') {
        # Place small 2x2 Games tile near the bottom edge of work_area so opening the 802+px Settings page
        # overflows below work_area unless fit_panel clamps it back inside the screen.
        $wa = [GfWinArea]::Get()
        $scale = [double]$wa[4] / 96.0
        $tx = $wa[0] + [int][Math]::Round(200 * $scale)
        $ty = $wa[3] - [int][Math]::Round(130 * $scale)
        $tw = [int][Math]::Round(108 * $scale)
        $th = [int][Math]::Round(114 * $scale)
        $cfgPath = Join-Path (Join-Path $dir 'Games') '.glassfolder.json'
        "{`"tile`":[$tx,$ty,$tw,$th]}" | Set-Content -Path $cfgPath -Encoding ASCII
    }
}

# Autostart test key (HKCU\<key>): never the real Run key.
function Remove-TestKey($key) {
    try { Remove-Item "HKCU:\$key" -Recurse -Force -ErrorAction Stop } catch {}
    $parent = 'HKCU:\Software\GlassFoldersUiTest'
    try {
        if ((Test-Path $parent) -and -not (Get-ChildItem $parent -ErrorAction Stop) -and -not (Get-Item $parent).Property) {
            Remove-Item $parent -Force -ErrorAction Stop
        }
    } catch {}
}

$fail = 0
foreach ($s in $scen) {
    $realSnap = Get-RealAutostart
    $tmp = Join-Path $env:TEMP "gf-test\$($s.BaseName)"
    New-Fixture (Join-Path $tmp 'desk')
    $fix = @(Get-Content $s.FullName -Encoding UTF8 | Where-Object { $_ -like '#fixture *' } | ForEach-Object { $_.Substring(9).Trim() })
    Add-Fixture (Join-Path $tmp 'desk') $fix
    $regKey = "Software\GlassFoldersUiTest\$($s.BaseName)"
    Remove-TestKey $regKey
    $cfg = Join-Path $tmp 'appdata\glass-folders'
    if (Test-Path $cfg) { Remove-Item $cfg -Recurse -Force }
    New-Item -ItemType Directory -Force $cfg | Out-Null
    # fixed settings: Russian UI, rest default (same pictures on any PC). Scenario line '#settings {json}' overrides.
    $set = (Get-Content $s.FullName -Encoding UTF8 | Where-Object { $_ -like '#settings *' } | Select-Object -First 1)
    $set = if ($set) { $set.Substring(10) } else { '{"lang":0}' }
    $set | Set-Content (Join-Path $cfg 'settings.json')
    $out = Join-Path $outAll $s.BaseName
    if (Test-Path $out) { Remove-Item $out -Recurse -Force }

    $psi = New-Object System.Diagnostics.ProcessStartInfo $exe
    $psi.UseShellExecute = $false
    $psi.EnvironmentVariables['GF_SCRIPT'] = $s.FullName
    $psi.EnvironmentVariables['GF_OUT'] = $out
    $psi.EnvironmentVariables['GF_DESKTOP'] = Join-Path $tmp 'desk'
    $psi.EnvironmentVariables['APPDATA'] = Join-Path $tmp 'appdata'
    $psi.EnvironmentVariables['GF_STAY'] = '1'
    $psi.EnvironmentVariables['GF_AUTOSTART_KEY'] = $regKey
    $p = [System.Diagnostics.Process]::Start($psi)
    if (-not $p.WaitForExit(90000)) { $p.Kill(); $code = 2 } else { $code = $p.ExitCode }
    Remove-TestKey $regKey
    $realAfter = Get-RealAutostart
    if ($realSnap.Exists -ne $realAfter.Exists -or $realSnap.Value -ne $realAfter.Value) {
        Restore-RealAutostart $realSnap
        Write-Output ("ERROR {0}: real HKCU Run\GlassFolders was mutated! Restored snapshot." -f $s.BaseName)
        $code = 3
    }
    $res = if ($code -eq 0) { 'PASS' } else { 'FAIL'; $fail++ }
    Write-Output ("{0,-5} {1}  (exit {2}, log {3})" -f $res, $s.BaseName, $code, (Join-Path $out 'log.txt'))
    if ($code -ne 0 -and (Test-Path (Join-Path $out 'log.txt'))) { Get-Content (Join-Path $out 'log.txt') -Tail 5 }
}
exit $fail

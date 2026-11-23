# Кратко: сколько секунд пользователь не трогал мышь/клавиатуру. Старые тесты с настоящей мышью обязаны
# вызвать это и остановиться, если человек за компьютером (меньше 120 с простоя).
#
# Usage:
#   powershell -File tests/ui/idle.ps1                -> prints idle seconds
#   powershell -File tests/ui/idle.ps1 -Require 120   -> exit 1 + message if idle < 120 s
# In a real-input script (SetCursorPos / mouse_event / CopyFromScreen), first line:
#   & "$PSScriptRoot\idle.ps1" -Require 120; if ($LASTEXITCODE) { exit 1 }
# Prefer headless: tests/ui/run.ps1 (GF_SCRIPT) never touches real input.

param([int]$Require = 0)

Add-Type -Namespace GfIdle -Name Native -MemberDefinition @'
[StructLayout(LayoutKind.Sequential)] public struct LASTINPUTINFO { public uint cbSize; public uint dwTime; }
[DllImport("user32.dll")] public static extern bool GetLastInputInfo(ref LASTINPUTINFO p);
[DllImport("kernel32.dll")] public static extern ulong GetTickCount64();
'@

$li = New-Object GfIdle.Native+LASTINPUTINFO
$li.cbSize = [System.Runtime.InteropServices.Marshal]::SizeOf($li)
[void][GfIdle.Native]::GetLastInputInfo([ref]$li)
# dwTime = low 32 bits of the tick count at last input
$now = [GfIdle.Native]::GetTickCount64()
$idle = [math]::Floor((([uint64]$now -band 0xFFFFFFFF) - $li.dwTime + 0x100000000) % 0x100000000 / 1000)

if ($Require -gt 0) {
    if ($idle -lt $Require) {
        Write-Output "ABORT: user active (idle $idle s < $Require s). Real-input test would take over the mouse. Use tests/ui/run.ps1 instead."
        exit 1
    }
    Write-Output "ok: idle $idle s"
    exit 0
}
Write-Output $idle

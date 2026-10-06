param(
    [string]$Exe = "D:\code\SecRelay\target\debug\secrelay-desktop.exe",
    [string[]]$AppArgs = @("--settings-only", "--settings-page", "2"),
    [string]$Out = "D:\code\SecRelay\artifacts\relay-settings.png",
    [int]$WaitSeconds = 14
)

$ErrorActionPreference = "Stop"
Add-Type -AssemblyName System.Drawing

# 本进程先声明 DPI 感知，否则窗口矩形与截图会被系统缩放，截出来的图不对齐。
Add-Type @"
using System;
using System.Runtime.InteropServices;
public class Dpi {
    [DllImport("user32.dll")] public static extern bool SetProcessDpiAwarenessContext(IntPtr value);
}
"@
[void][Dpi]::SetProcessDpiAwarenessContext([IntPtr]::new(-4))

Add-Type @"
using System;
using System.Runtime.InteropServices;
public class Snapshot {
    [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr hWnd);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr hWnd, int nCmdShow);
    [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr hWnd, out RECT lpRect);
    [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr hWnd, IntPtr after, int x, int y, int cx, int cy, uint flags);
    [StructLayout(LayoutKind.Sequential)] public struct RECT { public int Left, Top, Right, Bottom; }
}
"@

Get-Process -Name "secrelay-desktop" -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
Start-Sleep -Milliseconds 300

$proc = Start-Process -FilePath $Exe -ArgumentList $AppArgs -PassThru
try {
    Start-Sleep -Seconds $WaitSeconds
    $target = Get-Process -Name "secrelay-desktop" -ErrorAction SilentlyContinue |
        Where-Object { $_.MainWindowHandle -ne [IntPtr]::Zero } |
        Select-Object -First 1
    if ($null -eq $target) { throw "没有找到 SecRelay 的窗口" }

    $hwnd = $target.MainWindowHandle
    [void][Snapshot]::ShowWindow($hwnd, 5)
    [void][Snapshot]::SetWindowPos($hwnd, [IntPtr]::new(-1), 0, 0, 0, 0, 0x0001 -bor 0x0002 -bor 0x0040)
    [void][Snapshot]::SetForegroundWindow($hwnd)
    Start-Sleep -Milliseconds 1500

    $rect = New-Object Snapshot+RECT
    [void][Snapshot]::GetWindowRect($hwnd, [ref]$rect)
    Write-Host "标题：$($target.MainWindowTitle)  矩形 $($rect.Left),$($rect.Top) - $($rect.Right),$($rect.Bottom)"

    $bitmap = New-Object System.Drawing.Bitmap ($rect.Right - $rect.Left), ($rect.Bottom - $rect.Top)
    $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
    $graphics.CopyFromScreen($rect.Left, $rect.Top, 0, 0, $bitmap.Size)
    $bitmap.Save($Out, [System.Drawing.Imaging.ImageFormat]::Png)
    $graphics.Dispose()
    $bitmap.Dispose()
    Write-Host "已保存 $Out"

    [void][Snapshot]::SetWindowPos($hwnd, [IntPtr]::new(-2), 0, 0, 0, 0, 0x0001 -bor 0x0002)
}
finally {
    Get-Process -Name "secrelay-desktop" -ErrorAction SilentlyContinue | Stop-Process -Force -ErrorAction SilentlyContinue
}

# Generates icons/icon.ico — a 256px vault door over a Minecraft-green block.
# Run only when the icon needs regenerating; the .ico is committed.
Add-Type -AssemblyName System.Drawing

$size = 256
$bmp = New-Object System.Drawing.Bitmap($size, $size, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.SmoothingMode = 'AntiAlias'
$g.Clear([System.Drawing.Color]::Transparent)

# Rounded slate panel
$panel = New-Object System.Drawing.Drawing2D.GraphicsPath
$r = 44; $m = 12; $w = $size - 2 * $m
$panel.AddArc($m, $m, $r, $r, 180, 90)
$panel.AddArc($m + $w - $r, $m, $r, $r, 270, 90)
$panel.AddArc($m + $w - $r, $m + $w - $r, $r, $r, 0, 90)
$panel.AddArc($m, $m + $w - $r, $r, $r, 90, 90)
$panel.CloseFigure()
$g.FillPath((New-Object System.Drawing.SolidBrush([System.Drawing.Color]::FromArgb(255, 28, 32, 39))), $panel)
$g.DrawPath((New-Object System.Drawing.Pen([System.Drawing.Color]::FromArgb(255, 46, 52, 66), 5)), $panel)

# Vault door ring
$green = [System.Drawing.Color]::FromArgb(255, 90, 164, 105)
$ringPen = New-Object System.Drawing.Pen($green, 16)
$g.DrawEllipse($ringPen, 62, 62, 132, 132)

# Spokes
$spokePen = New-Object System.Drawing.Pen($green, 14)
$spokePen.StartCap = 'Round'; $spokePen.EndCap = 'Round'
$cx = 128; $cy = 128
foreach ($angle in 0, 45, 90, 135) {
  $rad = $angle * [Math]::PI / 180
  $dx = [Math]::Cos($rad) * 78; $dy = [Math]::Sin($rad) * 78
  $g.DrawLine($spokePen, ($cx - $dx), ($cy - $dy), ($cx + $dx), ($cy + $dy))
}

# Hub
$g.FillEllipse((New-Object System.Drawing.SolidBrush([System.Drawing.Color]::FromArgb(255, 28, 32, 39))), 96, 96, 64, 64)
$g.FillEllipse((New-Object System.Drawing.SolidBrush($green)), 110, 110, 36, 36)
$g.Dispose()

# Wrap the PNG in a single-image ICO (PNG-compressed icons need Vista+).
$ms = New-Object System.IO.MemoryStream
$bmp.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png)
$png = $ms.ToArray()
$out = New-Object System.IO.MemoryStream
$bw = New-Object System.IO.BinaryWriter($out)
$bw.Write([UInt16]0); $bw.Write([UInt16]1); $bw.Write([UInt16]1)   # reserved, type=icon, count
$bw.Write([Byte]0); $bw.Write([Byte]0)                              # 0 = 256px
$bw.Write([Byte]0); $bw.Write([Byte]0)                              # palette, reserved
$bw.Write([UInt16]1); $bw.Write([UInt16]32)                         # planes, bpp
$bw.Write([UInt32]$png.Length); $bw.Write([UInt32]22)               # size, offset
$bw.Write($png)
$bw.Flush()

[System.IO.File]::WriteAllBytes("$PSScriptRoot\icon.ico", $out.ToArray())
$bmp.Dispose()
Write-Host "Wrote icon.ico ($($out.Length) bytes)"

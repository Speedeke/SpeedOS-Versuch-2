# tools/usb_schreiben.ps1 -- Schreibt speedos-live.img ROH auf einen USB-Stick
#
# ACHTUNG: Das LOESCHT den Ziel-USB-Stick vollstaendig und unwiderruflich.
# Deshalb mit mehreren Sicherungen:
#   - laeuft NUR als Administrator (Rohzugriff auf die Platte),
#   - waehlt automatisch NUR eine USB-/Wechsel-Platte (BusType USB, keine
#     System-/Boot-Platte, < 512 GB) -- die interne NVMe kann nicht
#     getroffen werden,
#   - zeigt Modell + Groesse und verlangt eine getippte Bestaetigung.
#
# Aufruf: Rechtsklick auf die Datei -> "Mit PowerShell ausfuehren" schlaegt
# fehl (keine Admin-Rechte). Stattdessen:
#   1. Startmenue -> "PowerShell" -> Rechtsklick -> "Als Administrator
#      ausfuehren".
#   2. In das Projektverzeichnis wechseln, dann:  .\tools\usb_schreiben.ps1
#   (bei Bedarf einmalig:  Set-ExecutionPolicy -Scope Process Bypass )
#
# ASCII-only mit Absicht (PowerShell 5.1 liest .ps1 ohne BOM als ANSI).

param(
    [string]$Image = "speedos-live.img",
    [int]$DiskNummer = -1          # -1 = automatisch die einzige USB-Platte
)

$ErrorActionPreference = "Stop"

# --- 1. Administrator-Rechte pruefen ---
$pr = New-Object Security.Principal.WindowsPrincipal([Security.Principal.WindowsIdentity]::GetCurrent())
if (-not $pr.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
    Write-Host "FEHLER: Dieses Skript braucht Administrator-Rechte (Rohzugriff auf die Platte)." -ForegroundColor Red
    Write-Host "  -> PowerShell 'Als Administrator ausfuehren' und erneut starten." -ForegroundColor Yellow
    exit 1
}

# --- 2. Image finden ---
if (-not (Test-Path $Image)) { throw "Image nicht gefunden: $Image  (zuerst 'cargo image' ausfuehren)" }
$imgVoll = (Resolve-Path $Image).Path
$imgLen  = (Get-Item $imgVoll).Length
Write-Host "Image: $imgVoll ($([math]::Round($imgLen/1MB,1)) MiB)"

# --- 3. Ziel-Datentraeger bestimmen (mit strengen Sicherungen) ---
$kandidaten = Get-Disk | Where-Object {
    $_.BusType -eq 'USB' -and -not $_.IsSystem -and -not $_.IsBoot -and $_.Size -lt 512GB
}
if ($DiskNummer -ge 0) {
    $ziel = Get-Disk -Number $DiskNummer
} elseif (@($kandidaten).Count -eq 1) {
    $ziel = $kandidaten
} elseif (@($kandidaten).Count -eq 0) {
    throw "Keine USB-/Wechselplatte gefunden. Stick eingesteckt? Sonst -DiskNummer angeben."
} else {
    Write-Host "Mehrere USB-Platten gefunden -- bitte mit -DiskNummer <n> waehlen:" -ForegroundColor Yellow
    $kandidaten | Select-Object Number, FriendlyName, @{N='GB';E={[math]::Round($_.Size/1GB,1)}} | Format-Table -AutoSize
    exit 1
}

# Sicherungs-Wall: das gewaehlte Ziel MUSS ein USB-Wechsel-Medium sein.
if ($ziel.BusType -ne 'USB' -or $ziel.IsSystem -or $ziel.IsBoot -or $ziel.Size -ge 512GB) {
    Write-Host "ABBRUCH: Datentraeger $($ziel.Number) ist KEINE sichere USB-Zielplatte" -ForegroundColor Red
    Write-Host "  (BusType=$($ziel.BusType), System=$($ziel.IsSystem), Boot=$($ziel.IsBoot), $([math]::Round($ziel.Size/1GB,1)) GB)." -ForegroundColor Red
    exit 1
}

$gb = [math]::Round($ziel.Size/1GB,1)
Write-Host ""
Write-Host "==================== ZIEL-USB-STICK ====================" -ForegroundColor Cyan
Write-Host ("  Datentraeger : {0}" -f $ziel.Number)
Write-Host ("  Modell       : {0}" -f $ziel.FriendlyName)
Write-Host ("  Groesse      : {0} GB   (BusType {1})" -f $gb, $ziel.BusType)
Get-Partition -DiskNumber $ziel.Number -ErrorAction SilentlyContinue |
    Where-Object DriveLetter |
    ForEach-Object { Write-Host ("  Partition    : {0}:  {1}" -f $_.DriveLetter, $_.Type) }
Write-Host "========================================================" -ForegroundColor Cyan
Write-Host "ALLE DATEN auf diesem Stick werden GELOESCHT." -ForegroundColor Red

$antwort = Read-Host "Zum Fortfahren  LOESCHEN  eintippen (alles andere bricht ab)"
if ($antwort -ne "LOESCHEN") { Write-Host "Abgebrochen -- nichts veraendert."; exit 0 }

# --- 4. Partitionen entfernen (haengt die Volumes aus) ---
# WICHTIG: KEIN Set-Disk -IsOffline -- Wechselmedien (USB-Sticks) koennen
# gar nicht offline gesetzt werden ("Removable media cannot be set to
# offline"). Nach Clear-Disk ist der Stick partitionslos, also kann der
# Rohschreib-Zugriff darauf trotzdem gelingen, obwohl er online bleibt.
Write-Host "Bereite Datentraeger vor ..."
Clear-Disk -Number $ziel.Number -RemoveData -RemoveOEM -Confirm:$false
# Schreibschutz best-effort loesen (manche Sticks melden ihn); Fehler
# hier sind unkritisch:
Set-Disk -Number $ziel.Number -IsReadOnly $false -ErrorAction SilentlyContinue

# --- 5. Image ROH auf die Platte schreiben (sektor-ausgerichtet) ---
Write-Host "Schreibe Image ... (nicht abbrechen!)"
$dev = $null; $img = $null
try {
    $img = [System.IO.File]::OpenRead($imgVoll)
    $dev = [System.IO.File]::Open("\\.\PhysicalDrive$($ziel.Number)", 'Open', 'Write', 'ReadWrite')
    $buf = New-Object byte[] (4 * 1024 * 1024)
    $gesamt = 0L
    while (($n = $img.Read($buf, 0, $buf.Length)) -gt 0) {
        # Auf 512-Byte-Sektoren auffuellen (letzter Block):
        $schreib = $n
        if ($schreib % 512 -ne 0) {
            $rest = 512 - ($schreib % 512)
            [Array]::Clear($buf, $schreib, $rest)
            $schreib += $rest
        }
        $dev.Write($buf, 0, $schreib)
        $gesamt += $n
        Write-Progress -Activity "Schreibe SpeedOS auf USB" -Status "$([math]::Round($gesamt/1MB,1)) MiB" `
            -PercentComplete ([math]::Min(100, [int]($gesamt * 100 / $imgLen)))
    }
    $dev.Flush()
} finally {
    if ($dev) { $dev.Close() }
    if ($img) { $img.Close() }
}
Write-Progress -Activity "Schreibe SpeedOS auf USB" -Completed

# --- 6. Platte neu einlesen (kein Offline noetig, s. o.) ---
Update-Disk -Number $ziel.Number

Write-Host ""
Write-Host "FERTIG. SpeedOS wurde auf Datentraeger $($ziel.Number) ($($ziel.FriendlyName)) geschrieben." -ForegroundColor Green
Write-Host "Stick sicher auswerfen, in den Laptop stecken, im BIOS/UEFI vom USB booten." -ForegroundColor Green
Write-Host "BIOS: Secure Boot AUS, UEFI-Boot (nicht Legacy/CSM). Details: docs/usb-boot.md"

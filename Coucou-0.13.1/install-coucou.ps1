# Coucou for Windows - clean (re)install, with Ollama for the local AI engine.
#
# Double-click "Installer Coucou.bat" next to this file, or run:
#   powershell -ExecutionPolicy Bypass -File install-coucou.ps1
#
# What it does, in order, and nothing else:
#   1. quits Coucou if it is running (the installer can't replace a running exe)
#   2. runs the newest Coucou-Windows-*-setup.exe found in ..\release (per-user, no admin)
#   3. checks Ollama; offers to install it with winget if it is missing
#   4. offers to download a local model that reads text, PDFs and images
#   5. starts Coucou
#
# Your settings (%APPDATA%\Coucou) and API keys (Windows Credential Manager)
# are kept: installing over an existing Coucou is an in-place upgrade.

$ErrorActionPreference = 'Stop'
$here = Split-Path -Parent $MyInvocation.MyCommand.Path

function Say($text, $color = 'Gray') { Write-Host $text -ForegroundColor $color }
function Ask($question) {
    $a = Read-Host "$question [O/n]"
    return ($a -eq '' -or $a -match '^(o|oui|y|yes)$')
}

Say ''
Say '  Coucou - installation' 'Cyan'
Say '  ---------------------' 'Cyan'
Say ''

# 1. Quit Coucou --------------------------------------------------------------
$running = Get-Process -Name 'coucou' -ErrorAction SilentlyContinue
if ($running) {
    Say '> Fermeture de Coucou...'
    $running | Stop-Process -Force
    Start-Sleep -Seconds 1
}

# 2. Installer ----------------------------------------------------------------
$searchDirs = @($here, (Join-Path $here '..\release')) | Where-Object { Test-Path $_ }
$setup = Get-ChildItem -Path $searchDirs -Filter 'Coucou-Windows-*-setup.exe' -ErrorAction SilentlyContinue |
    Sort-Object { [version]($_.Name -replace '^Coucou-Windows-(.+)-setup\.exe$', '$1') } -Descending |
    Select-Object -First 1
if (-not $setup) {
    $setup = Get-ChildItem -Path $searchDirs -Filter 'Coucou-Windows-setup.exe' -ErrorAction SilentlyContinue | Select-Object -First 1
}
if (-not $setup) {
    Say "x Aucun installeur trouve a cote du script" 'Red'
    Read-Host 'Entree pour fermer'
    exit 1
}

# Downloaded files carry a "from the internet" mark that triggers SmartScreen;
# this file was built for you, so the mark is removed.
Unblock-File -Path $setup.FullName -ErrorAction SilentlyContinue

Say "> Installation de $($setup.Name)..."
$proc = Start-Process -FilePath $setup.FullName -ArgumentList '/S' -Wait -PassThru
if ($proc.ExitCode -ne 0) {
    Say "x L'installeur a echoue (code $($proc.ExitCode)). Relance-le a la main : $($setup.FullName)" 'Red'
    Read-Host 'Entree pour fermer'
    exit 1
}
Say '  OK - Coucou est installe.' 'Green'

# 3. Ollama -------------------------------------------------------------------
function Find-Ollama {
    $cmd = Get-Command ollama -ErrorAction SilentlyContinue
    if ($cmd) { return $cmd.Source }
    $default = Join-Path $env:LOCALAPPDATA 'Programs\Ollama\ollama.exe'
    if (Test-Path $default) { return $default }
    return $null
}

function Test-OllamaUp {
    try {
        Invoke-RestMethod -Uri 'http://localhost:11434/api/version' -TimeoutSec 3 | Out-Null
        return $true
    } catch { return $false }
}

Say ''
$ollama = Find-Ollama
if (-not $ollama) {
    Say '> Ollama (IA locale, gratuite, hors ligne) n''est pas installe.' 'Yellow'
    if ((Get-Command winget -ErrorAction SilentlyContinue) -and (Ask '  L''installer maintenant avec winget ?')) {
        winget install --id Ollama.Ollama -e --accept-source-agreements --accept-package-agreements
        $ollama = Find-Ollama
    } else {
        Say '  Tu peux l''installer plus tard depuis https://ollama.com/download' 'Gray'
    }
}

if ($ollama) {
    if (-not (Test-OllamaUp)) {
        Say '> Demarrage d''Ollama...'
        $app = Join-Path (Split-Path $ollama) 'ollama app.exe'
        if (Test-Path $app) { Start-Process $app } else { Start-Process $ollama -ArgumentList 'serve' -WindowStyle Hidden }
        for ($i = 0; $i -lt 20 -and -not (Test-OllamaUp); $i++) { Start-Sleep -Seconds 1 }
    }

    if (Test-OllamaUp) {
        Say '  OK - Ollama repond sur http://localhost:11434' 'Green'
        $models = @()
        try { $models = (Invoke-RestMethod -Uri 'http://localhost:11434/api/tags' -TimeoutSec 5).models } catch {}
        if (-not $models -or $models.Count -eq 0) {
            Say ''
            Say '> Aucun modele installe. Recommande : gemma3 (lit texte, PDF et images, ~3 Go).' 'Yellow'
            if (Ask '  Telecharger gemma3 maintenant ?') {
                & $ollama pull gemma3
            }
        } else {
            Say ("  Modeles installes : " + (($models | ForEach-Object { $_.name }) -join ', '))
        }
    } else {
        Say '  Ollama ne repond pas encore. Lance "Ollama" depuis le menu Demarrer.' 'Yellow'
    }
}

# 5. Start Coucou ------------------------------------------------------------
$exe = Join-Path $env:LOCALAPPDATA 'Coucou\coucou.exe'
if (Test-Path $exe) {
    Say ''
    Say '> Lancement de Coucou...'
    Start-Process $exe
}

Say ''
Say '  Termine.' 'Cyan'
Say '  Dans Coucou : icone Mochi (zone de notification) > Settings... > AI engine > Ollama (local).'
Say '  Puis glisse un PDF ou une image sur l''ile en haut de l''ecran > "Corriger".'
Say ''
Read-Host 'Entree pour fermer'

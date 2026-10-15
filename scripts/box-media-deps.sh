#!/usr/bin/env bash
# Media/document libraries for a local `process` box (F10): python-pptx, python-docx, openpyxl,
# Pillow, reportlab in a venv under the relay data dir. System python is left untouched; export
# ARBOS_MEDIA_VENV before starting the relay so agentd points the agent at this interpreter.
#
#   scripts/box-media-deps.sh [data-dir]      # default: $ARBOS_RELAY_DATA or ./.arbos-local/relay
#   export ARBOS_MEDIA_VENV=<data-dir>/media-venv
set -euo pipefail

DATA_DIR="${1:-${ARBOS_RELAY_DATA:-./.arbos-local/relay}}"
VENV="$DATA_DIR/media-venv"
PKGS=(python-pptx python-docx openpyxl Pillow reportlab)

mkdir -p "$DATA_DIR"
if [ ! -x "$VENV/bin/python" ]; then
  if command -v uv >/dev/null 2>&1; then
    uv venv "$VENV" --seed >/dev/null
  else
    python3 -m venv "$VENV"
  fi
fi
if command -v uv >/dev/null 2>&1; then
  uv pip install --python "$VENV/bin/python" --quiet "${PKGS[@]}"
else
  "$VENV/bin/python" -m pip install --quiet --upgrade pip
  "$VENV/bin/python" -m pip install --quiet "${PKGS[@]}"
fi
"$VENV/bin/python" - <<'PY'
import docx, openpyxl, pptx, PIL, reportlab
print("media venv ok: python-pptx", pptx.__version__, "python-docx", docx.__version__ if hasattr(docx, "__version__") else "", "openpyxl", openpyxl.__version__, "Pillow", PIL.__version__, "reportlab", reportlab.Version)
PY
command -v ffmpeg >/dev/null 2>&1 || echo "note: ffmpeg not found on PATH (apt install ffmpeg) — video/audio conversion will be unavailable" >&2
echo "export ARBOS_MEDIA_VENV=$(cd "$VENV" && pwd)"

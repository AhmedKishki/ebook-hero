#!/bin/sh
# Rebuilds the PDF fixtures from the .tex sources in this directory.
# Needs pdflatex (with hyperref, fancyhdr and multicol) and mutool.
set -eu
cd "$(dirname "$0")"

# Fixed dates make a rebuild produce the same bytes.
SOURCE_DATE_EPOCH=1700000000
FORCE_SOURCE_DATE=1
export SOURCE_DATE_EPOCH FORCE_SOURCE_DATE

latex() {
    pdflatex -interaction=nonstopmode -halt-on-error "$1.tex" >/dev/null
}

# Twice, so the bookmarks and page labels settle.
latex sample
latex sample
mv sample.pdf ..

# Page 3 of sample.pdf, drawn as a picture, stands in for a scanned page.
mutool draw -q -r 72 -o page.png ../sample.pdf 3 2>/dev/null

latex imageonly
latex mixed
mv imageonly.pdf mixed.pdf ..

# The same book behind a user password, to test the password check.
# AES uses a random initialisation vector, so this file differs on every build.
mutool clean -E aes-128 -U secret ../sample.pdf ../locked.pdf

rm -f -- *.aux *.log *.out

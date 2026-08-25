#!/usr/bin/env bash
# Fetch the 12 product images for the "nile" shop-demo from LoremFlickr (free,
# keyword-matched, Creative Commons). Locks pin a specific photo where one was
# hand-picked; un-locked tags return a random matching photo each run.
#
# NOTE ON LICENSING: LoremFlickr serves Flickr photos under various CC licenses
# (some NC / ND, with attribution watermarks). They are fine for LOCAL demo use
# but are intentionally NOT committed to this repo. For a shipped/redistributed
# demo, replace these with CC0 or your own product photos (same 1.jpg … 12.jpg
# names). If an image is missing, the product card falls back to an emoji.
set -e
cd "$(dirname "$0")/public/img"

dl() { curl -sL -o "$1.jpg" "https://loremflickr.com/500/500/$2/all${3:+?lock=$3}"; echo "  $1 <- $2 ${3:+(lock $3)}"; sleep 0.4; }

dl 1  wireless,headphones
dl 2  typing,keyboard      12
dl 3  webcam,camera
dl 4  armchair             8
dl 5  harddisk,drive       11
dl 6  standing,desk
dl 7  espresso,machine
dl 8  robot,vacuum         5
dl 9  air,purifier
dl 10 kettle,teapot        2
dl 11 computer,monitor     40
dl 12 bluetooth,speaker

echo "done — 12 images in $(pwd)"

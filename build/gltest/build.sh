#!/bin/sh
# Cross-build gltest for the Surface (armv7h) in the Mesa build image.
set -e
cd "$(dirname "$0")"
docker run --rm --platform linux/arm/v7 -v "$PWD":/g srt-mesa-build \
  sh -c 'gcc -O2 -Wall -o /g/gltest /g/gltest.c -lEGL -lGLESv2 &&
         gcc -O2 -Wall -o /g/shadercheck /g/shadercheck.c -lEGL -lOpenGL'
file gltest shadercheck

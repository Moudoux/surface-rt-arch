#!/bin/sh
# Cross-build gltest for the Surface (armv7h) in the Mesa build image.
set -e
cd "$(dirname "$0")"
docker run --rm --platform linux/arm/v7 -v "$PWD":/g srt-mesa-build \
  sh -c 'gcc -O2 -Wall -o /g/gltest /g/gltest.c -lEGL -lGLESv2 &&
         gcc -O2 -Wall -o /g/shadercheck /g/shadercheck.c -lEGL -lOpenGL &&
         gcc -O2 -Wall -I/usr/include/libdrm -o /g/dmabuftest /g/dmabuftest.c -lEGL -lGLESv2 -lgbm &&
         gcc -O2 -Wall -o /g/batchtest /g/batchtest.c -lEGL -lGLESv2 -lgbm &&
         gcc -O2 -Wall -o /g/fillbench /g/fillbench.c -lEGL -lGLESv2'
file gltest shadercheck

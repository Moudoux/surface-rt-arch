FROM debian:trixie
RUN apt-get update && DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
    gcc libc6-dev gcc-arm-none-eabi libnewlib-arm-none-eabi pkg-config gcc-arm-linux-gnueabihf g++-arm-linux-gnueabihf binutils-arm-linux-gnueabihf \
    make bc bison flex libssl-dev libelf-dev cpio kmod xz-utils zstd lz4 \
    device-tree-compiler python3 python3-setuptools python3-dev python3-pyelftools swig \
    git patch rsync ca-certificates libgnutls28-dev uuid-dev u-boot-tools \
    e2fsprogs dosfstools mtools fdisk libarchive-tools file \
    && rm -rf /var/lib/apt/lists/*
ARG UID=1000
ARG GID=1000
RUN groupadd -g $GID builder && useradd -m -u $UID -g $GID builder
USER builder
WORKDIR /work

# Stage 1: crosstool-ng environment
# Use --target ctng to get this stage (for menuconfig, rebuilding toolchain)
FROM ubuntu:latest AS ctng

RUN apt-get update && apt-get install -y \
    gcc g++ gperf bison flex texinfo help2man make \
    libncurses5-dev python3-dev autoconf automake \
    libtool libtool-bin gawk wget bzip2 xz-utils \
    unzip patch libstdc++6 rsync git \
    && rm -rf /var/lib/apt/lists/*

RUN useradd -ms /bin/bash ctng
USER ctng
WORKDIR /home/ctng

RUN git clone https://github.com/crosstool-ng/crosstool-ng.git \
    && cd crosstool-ng \
    && ./bootstrap \
    && ./configure --prefix=/home/ctng/.local \
    && make && make install

ENV PATH="/home/ctng/.local/bin:$PATH"

# Copy .config and build the toolchain (cached unless .config changes)
COPY --chown=ctng:ctng .config /home/ctng/crosstool/.config
RUN cd /home/ctng/crosstool && ct-ng build

# Stage 2: slim compiler image for everyday use
FROM ubuntu:latest AS compiler

RUN apt-get update && apt-get install -y make \
    && rm -rf /var/lib/apt/lists/*

RUN useradd -ms /bin/bash ctng
COPY --from=ctng /home/ctng/x-tools /home/ctng/x-tools
RUN chown -R ctng:ctng /home/ctng/x-tools

ENV PATH="/home/ctng/x-tools/x86_64-unknown-linux-musl/bin:$PATH"
USER ctng
WORKDIR /home/ctng

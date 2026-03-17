FROM ubuntu:latest

RUN apt-get update && apt-get install -y \
    gcc g++ gperf bison flex texinfo help2man make \
    libncurses5-dev python3-dev autoconf automake \
    libtool libtool-bin gawk wget bzip2 xz-utils \
    unzip patch libstdc++6 rsync git gosu \
    && rm -rf /var/lib/apt/lists/*

RUN useradd -ms /bin/bash ctng
RUN mkdir -p /home/ctng/x-tools /home/ctng/src /home/ctng/crosstool
USER ctng
WORKDIR /home/ctng

RUN git clone https://github.com/crosstool-ng/crosstool-ng.git \
    && cd crosstool-ng \
    && ./bootstrap \
    && ./configure --prefix=/home/ctng/.local \
    && make && make install

ENV PATH="/home/ctng/.local/bin:$PATH"

# Copy .config into the container's case-sensitive filesystem
COPY --chown=ctng:ctng .config /home/ctng/crosstool/.config

USER root
COPY entrypoint.sh /entrypoint.sh
RUN chmod +x /entrypoint.sh

ENTRYPOINT ["/entrypoint.sh"]
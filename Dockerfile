FROM ubuntu:22.04

RUN apt-get update && apt-get install -y \
    libssl-dev libudev-dev pkg-config zlib1g-dev \
    llvm clang cmake make libprotobuf-dev protobuf-compiler \
    curl git

# Install Rust
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
ENV PATH="/root/.cargo/bin:${PATH}"

WORKDIR /app
# Clone the repo during build or mount it as a volume
#
# RUN git clone -b zed/txbundle https://github.com/defnotzed/atlas-txn-sender.git .
# RUN git clone https://github.com/defnotzed/atlas-txn-sender.git .

EXPOSE 4040

 CMD ["cargo", "run", "--release"]
FROM rustlang/rust:nightly-bookworm-slim@sha256:8a274febcfdc03286cb35e63afd808be181e10e9bc4e60d5a13c054fa8ad0188 AS builder

WORKDIR /usr/src/koebot
COPY . .

RUN apt-get update \
    && apt-get install -y curl ffmpeg python3 cmake libopus-dev pkg-config libssl-dev

RUN cargo install --locked --path .

FROM debian:bookworm-slim@sha256:f06537653ac770703bc45b4b113475bd402f451e85223f0f2837acbf89ab020a

ARG YT_DLP_VERSION=2026.08.19
ARG YT_DLP_SHA256=1fa6733c37ea6fb51c99ad8fe785e7b7e5f3246c9b980230329d4fb72ed8d4d6

RUN apt-get update \
    && apt-get install -y curl ffmpeg python3 libopus-dev \
    && rm -rf /var/lib/apt/lists/*

RUN curl -fsSL "https://github.com/yt-dlp/yt-dlp/releases/download/${YT_DLP_VERSION}/yt-dlp" -o /usr/local/bin/yt-dlp \
    && echo "${YT_DLP_SHA256}  /usr/local/bin/yt-dlp" | sha256sum --check --strict \
    && chmod +x /usr/local/bin/yt-dlp
RUN ln -vsf /usr/bin/python3 /usr/bin/python

COPY --from=builder /usr/local/cargo/bin/koebot /usr/local/bin/koebot

CMD ["koebot"]

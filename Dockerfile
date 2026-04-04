FROM rustlang/rust:nightly-bookworm-slim@sha256:8a274febcfdc03286cb35e63afd808be181e10e9bc4e60d5a13c054fa8ad0188 AS builder

WORKDIR /usr/src/koebot
COPY . .

RUN apt-get update \
    && apt-get install -y curl ffmpeg python3 cmake libopus-dev pkg-config libssl-dev

RUN cargo install --path .

FROM debian:bookworm-slim@sha256:f06537653ac770703bc45b4b113475bd402f451e85223f0f2837acbf89ab020a

RUN apt-get update \
    && apt-get install -y curl ffmpeg python3 libopus-dev \
    && rm -rf /var/lib/apt/lists/*

RUN curl -sSL https://github.com/yt-dlp/yt-dlp/releases/download/2026.03.17/yt-dlp -o /usr/local/bin/yt-dlp && chmod +x /usr/local/bin/yt-dlp
RUN ln -vsf /usr/bin/python3 /usr/bin/python

COPY --from=builder /usr/local/cargo/bin/koebot /usr/local/bin/koebot

CMD ["koebot"]

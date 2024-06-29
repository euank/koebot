FROM rust:1.79.0-bookworm as builder

WORKDIR /usr/src/koebot
COPY . .

RUN apt-get update \
    && apt-get install -y curl ffmpeg python3 cmake libopus-dev

RUN cargo install --path .

FROM debian:bookworm-slim

RUN apt-get update \
    && apt-get install -y curl ffmpeg python3 libopus-dev \
    && rm -rf /var/lib/apt/lists/*

RUN curl -sSL https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp -o /usr/local/bin/yt-dlp && chmod +x /usr/local/bin/yt-dlp
RUN ln -vsf /usr/bin/python3 /usr/bin/python

COPY --from=builder /usr/local/cargo/bin/koebot /usr/local/bin/koebot

CMD ["koebot"]

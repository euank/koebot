FROM rust:1.61-buster as builder

WORKDIR /usr/src/koebot
COPY . .

RUN cargo install --path .

FROM debian:buster-slim

RUN apt-get update \
    && apt-get install -y curl ffmpeg python3 \
    && rm -rf /var/lib/apt/lists/*

RUN curl -sSL https://github.com/yt-dlp/yt-dlp/releases/latest/download/yt-dlp -o /usr/local/bin/youtube-dl && chmod +x /usr/local/bin/youtube-dl
RUN ln -vsf /usr/bin/python3 /usr/bin/python

COPY --from=builder /usr/local/cargo/bin/koebot /usr/local/bin/koebot

CMD ["koebot"]

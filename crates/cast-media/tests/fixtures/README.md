`h264-aac.ts` is the one-second synthetic H.264/AAC fixture described in
`crates/remux/tests/fixtures/README.md`, with B frames disabled, packet-copied
into MPEG-TS using `ffmpeg -i fixture.mp4 -c copy -mpegts_flags +resend_headers
-f mpegts h264-aac.ts`. It is used to verify real native codec probing before
finite HLS playlists are exposed to receivers.

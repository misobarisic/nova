`h264-aac.mkv` is a synthetic one-second 64×48 test pattern with a 440 Hz sine,
12 fps H.264 and AAC. The main Matroska fixture uses `-bf 0`;
`h264-aac-bframes.mp4` and `h264-aac-missing-dts.mkv` use `-bf 2`.
The latter deliberately demonstrates rejection of ambiguous initial DTS. Generated locally with:

```sh
ffmpeg -f lavfi -i testsrc2=size=64x48:rate=12 \
  -f lavfi -i sine=frequency=440:sample_rate=44100 -t 1 \
  -c:v libx264 -pix_fmt yuv420p -bf 2 -g 12 -c:a aac -b:a 32k fixture.mp4
ffmpeg -i fixture.mp4 -c copy h264-aac.mkv
```

The production library uses shared FFmpeg libraries directly and never launches
an executable or opens an encoder/decoder. Tests compare compressed packet
fingerprints across Matroska → finalized MP4 and exercise its custom AVIO path.

`h264-aac-offset.mp4` is six seconds of generated testsrc2 (64×48, 12 fps,
H.264 with B frames and one-second keyframe intervals) and a 440 Hz sine AAC
track, offset seven seconds. It exercises multiple RAM fragments, shared A/V
timestamps, and input keyframe resume with a nonzero container start time.

`aac-only.m4a` copies just the generated AAC track from that six-second fixture
for audio-only streaming and duration-based fragment boundary coverage.

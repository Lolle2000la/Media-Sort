# Decode fixtures for the AppImage gate

Real bitstreams for the codecs the AppImage promises to preview. `verify.sh`
checks each file's `codec_name` and then decodes every frame of it with the
**bundled** `ffmpeg` (`-v error -i <file> -f null -`, failing on any stderr line
or non-zero exit), so a payload whose H.264/HEVC/VP9 decoder was dropped or
broken by a build-flag change fails the gate instead of shipping.

The AV1 proof stays where it was: `resources/MockState/mock 3.mp4` decoded with
an explicit `-c:v libdav1d`, since FFmpeg has a native AV1 decoder that would
otherwise hide a missing/dysfunctional libdav1d.

These files are **generated outside the payload**. The bundled FFmpeg is built
without `--enable-libx264`/`libx265`/`libvpx-vp9` on purpose (nothing in the app
encodes video); only the generating machine needs those encoders, i.e. a normal
desktop `ffmpeg` build.

## Fixtures

| File | Codec | Size | Container |
| --- | --- | --- | --- |
| `h264.mp4` | H.264 (libx264) | ~5.4 KB | MP4 |
| `hevc.mp4` | H.265/HEVC (libx265) | ~6.3 KB | MP4 |
| `vp9.webm` | VP9 (libvpx-vp9) | ~12.3 KB | WebM |

All three are 320x180, 25 fps, 1 s of `testsrc2`, `yuv420p`, no audio, no
metadata, and encoded at a deliberately high CRF so the whole directory stays
under 25 KB. The point is that they decode, not that they look good.

## Regenerating

From this directory, with an `ffmpeg` that has `libx264`, `libx265` and
`libvpx-vp9` (check with `ffmpeg -hide_banner -encoders | grep -E 'libx264|libx265|libvpx-vp9'`):

```bash
ffmpeg -hide_banner -loglevel error -y \
  -f lavfi -i testsrc2=size=320x180:rate=25 -t 1 \
  -c:v libx264 -preset veryslow -crf 51 -pix_fmt yuv420p \
  -an -map_metadata -1 -fflags +bitexact -flags:v +bitexact \
  -movflags +faststart h264.mp4

ffmpeg -hide_banner -loglevel error -y \
  -f lavfi -i testsrc2=size=320x180:rate=25 -t 1 \
  -c:v libx265 -preset veryslow -crf 51 -pix_fmt yuv420p \
  -x265-params log-level=error \
  -an -map_metadata -1 -fflags +bitexact -flags:v +bitexact \
  -movflags +faststart hevc.mp4

ffmpeg -hide_banner -loglevel error -y \
  -f lavfi -i testsrc2=size=320x180:rate=25 -t 1 \
  -c:v libvpx-vp9 -crf 50 -b:v 0 -deadline good -cpu-used 0 -pix_fmt yuv420p \
  -an -map_metadata -1 -fflags +bitexact -flags:v +bitexact \
  vp9.webm
```

`-map_metadata -1` and `-fflags +bitexact -flags:v +bitexact` keep the muxer
from stamping encoder versions and creation dates into the files, so a
regeneration on the same ffmpeg produces the same bytes and a diff stays
reviewable.

Verify the result:

```bash
for f in h264.mp4 hevc.mp4 vp9.webm; do
  ffprobe -v error -select_streams v:0 \
    -show_entries stream=codec_name,width,height,pix_fmt -of default=nw=1 "$f"
done
```

If an encoder is unavailable (some distro ffmpeg is built without x264/x265),
any equivalent encode with the same codec/container works — the gate asserts the
`codec_name` and the decode, not the encoder that produced the file.
